//! The instance's path to its workspace, without the toolkit: what the runtime planned is committed
//! to the workspace document and handed to the workspace through the collab client (`collaboration`), and
//! what the workspace's other replicas committed comes back into the document and the instance.
//!
//! An instance queues each planned change as an invocation of the host's `commit` action
//! ([`Instance::take_invocation`]). [`InstanceDocument::commit`] applies each to the workspace document as
//! the recorded action it is ([`DynAction`]), attributed to the person at the window, and sends
//! the change on the document's route. [`InstanceDocument::receive`] takes in the changes the route
//! brings, checked against the route they came on, and brings the instance up to date with the
//! document.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::Arc;

use app_state::{DocStore, DocWriter};
use automerge::ChangeHash;
use collab_types::document::DocumentId;
use collaboration::document::ResourceSaveStatus;
use collaboration::document::save::{open_stored, resource_document};
use collaboration::document_content::{ContentLink, DocumentContent};
use collaboration::document_routes::{decode_routed_mutation, validated_route_payload_with};
use collaboration::history_source::steps::{self, Scope};
use collaboration::history_source::verbs::{self, HistoryEdit};
use collaboration::{
    CollabEvent, CollabRequest, CollaborationManager, DocumentMutation, RouteSpec,
};
use durable_store::DurableStore;
use tilo_content::ContentStore;

use serde_json::Value as Json;
use tilo_actions::{ActionContext, ActionInvoker, EncodedAction, InvocationId};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::Doc;

use super::Instance;
use crate::action::DynAction;
use crate::chunk::{self, ChunkNo, Chunks, FIRST, Stamps};
pub use crate::history::{HostHistoryEdit, HostWords, register};
use crate::publish::{COMMIT, HOST_RESOURCE};

/// Why a change did not reach the workspace.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    /// The instance has no host over a document yet.
    #[error("the instance holds no workspace document yet")]
    NoHost,
    /// No committed action is queued under the ticket.
    #[error("no committed action {0}")]
    NoAction(u64),
    /// The action could not be applied to the workspace document.
    #[error("the action was refused: {0}")]
    Refused(String),
    /// The document could not be opened: no route, an unreadable copy, or a committed read that
    /// failed.
    #[error("the workspace document could not be opened: {0}")]
    Open(String),
}

/// Where the broker's committed read of a document being opened comes from.
enum Read {
    /// Read on this thread, as the document opens.
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
/// Its history is the one every app's document has (`collaboration::history_source`): each change
/// is a step of whoever made it, and Undo, Redo, History and the other history verbs are
/// [`verbs`], applied to the document as steps of their own, as a native app applies them.
pub struct InstanceDocument {
    /// The document (chunk [`FIRST`]) and the chunks above it this replica holds, each a document of
    /// its own. A change is written in the chunk the manifest places its resource in, and one
    /// action's changes across chunks travel under one workspace commit. Once the instance's
    /// store opens over them they are the store's, the one copy of each: the document reads and
    /// writes them through it ([`Docs`]).
    chunks: Docs,
    /// Whether a write since the instance last took the document in moved what the store reads.
    moved: bool,
    manager: CollaborationManager,
    route: RouteSpec,
    /// The kind of the document, as its app publishes it: what every route of it is joined as.
    kind: String,
    person: String,
    /// The heads of what the workspace was handed, per chunk: a commit sends what follows them.
    /// They move only once the collab client took the change; what it could not take yet (its
    /// queue full, the route still connecting) stays in the document past them and goes with the
    /// next send.
    sent: Stamps,
    /// The chunks above the document whose routes this document joined: a chunk's changes travel on
    /// `derived:chunk.<n>@<base>`, and only a replica that opened it is sent them.
    joined: BTreeSet<ChunkNo>,
    /// Why the document's changes past [`Self::sent`] stay on this device: the collab client
    /// refused them for good (a change above its size limit). They are kept in the document and
    /// its on-disk copy, never dropped.
    unshared: Option<String>,
    /// While the workspace refuses the document's changes: the heads of each chunk as last handed
    /// to it. Every change after a refused one builds on it, so the workspace takes none of them
    /// alone: [`Self::sent`] is cleared, and the document goes whole once the person makes another
    /// step or asks to try again, and not before ([`Self::send`]).
    refused: Option<Stamps>,
    /// Why the workspace's copy of the document could not be read here, waiting for the window to
    /// say it: the route went on with the document this device holds, and nothing says it twice.
    unreadable: Option<String>,
    /// The words its history verbs speak in.
    words: HostWords,
    /// The on-disk copies of the chunks this replica holds, each a document of its own written
    /// as the document's own is; `None` keeps them in memory only.
    writers: Option<Writers>,
    /// Whether this device governs the workspace: the document's origin (convergence.md §1), whose
    /// copy needs no committed read from another host.
    governs: bool,
    /// The document's content store ([`Self::content`]), and the content link it meets the document's route
    /// in: what it keeps is offered on the route, and what it lacks is asked of it.
    content: ContentStore,
    link: Arc<ContentLink>,
    /// Whether the document runs on this device alone ([`Self::open_local`]): there is no workspace
    /// to hand its changes to, so they are kept on disk and counted as handed over.
    local: bool,
}

/// The document's chunk documents: the document's own until the instance's store opens over them,
/// and the store's from then on, so each is held once. Every write goes through the store
/// ([`crate::store::TiloStore::write_document`]), which takes its unsaved writes out first and
/// derives what the write changed after.
enum Docs {
    Own(Chunks),
    Store(crate::store::TiloStore),
}

impl Docs {
    fn read<R>(&self, read: impl FnOnce(&Chunks) -> R) -> R {
        match self {
            Self::Own(chunks) => read(chunks),
            Self::Store(store) => store.with_document(read),
        }
    }

    /// Write the chunks; whether what the store reads moved.
    fn write_chunk<R>(&mut self, chunk: ChunkNo, write: impl FnOnce(&mut Doc) -> R) -> (R, bool) {
        match self {
            Self::Own(chunks) => (write(chunks.open_held(chunk)), true),
            Self::Store(store) => store.write_chunk(chunk, write),
        }
    }

    /// Read the document's manifest again, after a change reached it: the store does so as it
    /// takes the change in.
    fn reread_placement(&mut self) {
        if let Self::Own(chunks) = self {
            chunks.reread_placement();
        }
    }
}

/// The on-disk copies of the document's chunks: the store the document was opened from, and the
/// writer of each chunk it holds. Chunk [`FIRST`] is the document's own copy, which the document
/// opens; every chunk above it is a document of its own, kept beside the document's.
struct Writers {
    /// Where the copies live: what a chunk this replica comes to hold later is kept in.
    documents: Rc<dyn DurableStore>,
    /// The writer of each chunk this replica holds, the document first.
    chunks: BTreeMap<ChunkNo, DocWriter>,
}

impl Writers {
    /// The copies kept in `documents`, with the writer the document's own chunk
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

/// The document the copy of chunk `chunk` of `route`'s document is kept in, in `documents`: the
/// document of the chunk above the document ([`DocumentId::chunk`]), a document of its own beside the
/// document's copy. `None` for the document's own chunk, which the document itself opens, and for a
/// route whose document is no document id.
fn copy_document(
    documents: &Rc<dyn DurableStore>,
    route: &RouteSpec,
    chunk: ChunkNo,
) -> Option<DocStore> {
    if chunk == FIRST {
        return None;
    }
    let base = DocumentId::from_route(route.document.as_deref().unwrap_or(""))?;
    let key = base.chunk(chunk).storage_key();
    resource_document(
        documents,
        &route.app_id,
        &route.cluster_id.to_string(),
        &key,
    )
    .ok()
}

/// The document `route`'s document is kept in, in `documents`: the workspace's document of the app,
/// or the one the route names, a member's own.
fn route_document(
    documents: &Rc<dyn DurableStore>,
    route: &RouteSpec,
) -> Result<DocStore, DocumentError> {
    let workspace = route.cluster_id.to_string();
    match &route.document {
        None => {
            collaboration::document::save::workspace_document(documents, &route.app_id, &workspace)
        }
        Some(record) => {
            let id = DocumentId::parse(record)
                .ok_or_else(|| DocumentError::Open(format!("`{record}` is no document id")))?;
            resource_document(documents, &route.app_id, &workspace, &id.storage_key())
        }
    }
    .map_err(|error| DocumentError::Open(error.to_string()))
}

type ProfileArchive = (Doc, Vec<(ChunkNo, Doc)>);

/// Recover the complete local archive for the same person and workspace context.
fn previous_profile_document(
    documents: &Rc<dyn DurableStore>,
    route: &RouteSpec,
    person: &str,
) -> Result<Option<ProfileArchive>, DocumentError> {
    if collab_types::personal_cluster_of(person) != Some(route.cluster_id) {
        return Ok(None);
    }
    let Some(context) = route
        .document
        .as_deref()
        .and_then(collab_types::person_document_context)
    else {
        return Ok(None);
    };
    let Some(id) = crate::member_document_of(&route.app_id, person) else {
        return Ok(None);
    };
    let previous = RouteSpec::document(route.app_id.clone(), context, id);
    let document = route_document(documents, &previous)?
        .load_complete()
        .map_err(|error| DocumentError::Open(error.to_string()))?;
    let Some(document) = document else {
        return Ok(None);
    };
    let placement = Chunks::of(document.clone()).placement().chunks();
    let mut parts = Vec::with_capacity(placement.len());
    for index in placement {
        let copy = copy_document(documents, &previous, index)
            .and_then(|mut store| store.load_complete().ok().flatten())
            .ok_or_else(|| {
                DocumentError::Open(
                    "the previous profile document has a part on another device".to_owned(),
                )
            })?;
        parts.push((index, copy));
    }
    Ok(Some((document, parts)))
}

/// The document `doc` of `route`, kept by `writer`, with every chunk its manifest names that this
/// device kept a copy of in `documents`: what was written into one here and not handed to the
/// workspace yet is still there, and what comes next reaches it as any change does.
fn held_chunks(
    documents: &Rc<dyn DurableStore>,
    route: &RouteSpec,
    doc: Doc,
    writer: DocWriter,
) -> (Chunks, Writers) {
    let mut writers = Writers::of(documents, writer);
    let mut chunks = Chunks::of(doc);
    let named: Vec<ChunkNo> = chunks.placement().chunks().into_iter().collect();
    for chunk in named {
        if let Some((copy, writer)) = writers.held(route, chunk) {
            chunks.insert(chunk, copy);
            writers.chunks.insert(chunk, writer);
        }
    }
    (chunks, writers)
}

impl InstanceDocument {
    /// The document `doc`, shared on `route` through `manager`, changed by `person`.
    #[must_use]
    pub fn new(
        doc: Doc,
        manager: CollaborationManager,
        route: RouteSpec,
        person: impl Into<String>,
    ) -> Self {
        let kind = format!("tl.{}", route.app_id);
        Self::of(Chunks::of(doc), manager, route, person, kind)
    }

    /// The document `chunks` (the document's own document, chunk [`FIRST`], and whatever else this
    /// replica already holds), shared on `route` through `manager`, changed by `person`.
    #[must_use]
    pub fn of(
        chunks: Chunks,
        manager: CollaborationManager,
        route: RouteSpec,
        person: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        let mut record = Self::holding(chunks, None, manager, route, kind.into(), person.into());
        record.join_chunks();
        record
    }

    fn holding(
        chunks: Chunks,
        writers: Option<Writers>,
        manager: CollaborationManager,
        route: RouteSpec,
        kind: String,
        person: String,
    ) -> Self {
        let mut chunks = chunks;
        // What the document's manifest places is where this replica routes its reads and writes.
        chunks.reread_placement();
        // The workspace holds everything each chunk this replica opens holds: only what it makes
        // past then is sent.
        let sent = chunks
            .docs()
            .map(|(chunk, doc)| (chunk, doc.committed_heads()))
            .collect();
        // The document's own content link: its wants and what arrives for it are this document's, whichever
        // other workspace's document of the app this process holds. Its content is sealed under the
        // secret of the members who read the document, once its route holds their keys.
        let link = Arc::new(ContentLink::default());
        ContentLink::show_on_route(&route, &link);
        Self {
            content: DocumentContent::store(ContentStore::memory(), Arc::clone(&link)),
            link,
            chunks: Docs::Own(chunks),
            moved: true,
            manager,
            route,
            kind,
            person,
            sent,
            joined: BTreeSet::new(),
            unshared: None,
            refused: None,
            unreadable: None,
            words: HostWords::new("en-US"),
            writers,
            governs: true,
            local: false,
        }
    }

    /// Whether this device governs the workspace, as the document's origin: a document another host
    /// governs is read from it first ([`Self::open`]). What the origin alone does (a package's
    /// migrations) runs where this holds.
    #[must_use]
    pub const fn governs(&self) -> bool {
        self.governs
    }

    /// Whether the document runs on this device alone ([`Self::open_local`]): nothing it publishes
    /// has a broker to go to.
    #[must_use]
    pub const fn is_local(&self) -> bool {
        self.local
    }

    /// The document, its history verbs speaking `locale` (`en-US`, `de-DE`).
    #[must_use]
    pub fn speaking(mut self, locale: impl Into<String>) -> Self {
        self.speak(locale);
        self
    }

    /// The document's history verbs speak `locale` from now on.
    pub fn speak(&mut self, locale: impl Into<String>) {
        self.words.locale = locale.into();
    }

    /// The document's history verbs name the actions `resources` publish by their labels.
    pub fn learn_labels(&mut self, resources: &[tilo_actions::ResourceSpec]) {
        std::sync::Arc::make_mut(&mut self.words.labels).learn(resources);
    }

    /// The words the document's history verbs speak in.
    #[must_use]
    pub fn words(&self) -> &HostWords {
        &self.words
    }

    /// The document, its content kept in `local` (the device's content store): what its actions
    /// keep as content outlives the instance, and is read back when the document opens again.
    #[must_use]
    pub fn keeping_content_in(mut self, local: ContentStore) -> Self {
        self.content = DocumentContent::store(local, Arc::clone(&self.link));
        self
    }

    /// The document's content store: where an action too large to record whole keeps its large
    /// parts, sealed for the document's readers, and where they are read back from on every replica.
    /// What it keeps is offered on the document's route, and what this device lacks is asked of it,
    /// as a native window's document content is. The window that commits to the document registers it
    /// for its code ([`tilo_actions::content::register`]).
    #[must_use]
    pub fn content(&self) -> &ContentStore {
        &self.content
    }

    /// Send on the document's route what its content store asked for and what it kept since: the
    /// content a change names reaches the document's host with the change, and what a change here
    /// names and this device lacks is fetched. The store wakes the window's code when it has more.
    fn send_content(&mut self) {
        self.link.attach(tilo_wake::WakeHandle::new());
        let requests = self.link.take_requests();
        for hash in requests.wants {
            if let Err(error) = self
                .manager
                .try_send(CollabRequest::WantContent { hash: hash.clone() })
            {
                tracing::debug!(%error, "workspace: a content request waits for the route");
                self.link.retry_want(hash);
            }
        }
        for (hash, bytes) in requests.offers {
            match self.manager.try_send(CollabRequest::OfferContent {
                hash: hash.clone(),
                bytes: bytes.clone(),
            }) {
                Ok(()) | Err(collaboration::CollabSendError::TooLarge { .. }) => {}
                Err(_) => self.link.retry_offer(hash, bytes),
            }
        }
    }

    /// Open the workspace document of the route `manager` joined, changed by `person`, kept in
    /// `documents`: as a native launch opens its workspace's document. In a workspace this device governs,
    /// the document is its on-disk copy. In a workspace another host governs, the broker's committed
    /// read of the workspace comes first and is merged into that copy, which keeps it from then on.
    pub fn open(
        manager: CollaborationManager,
        documents: &Rc<dyn DurableStore>,
        person: impl Into<String>,
        kind: impl Into<String>,
    ) -> Result<Self, DocumentError> {
        Self::open_with(manager, Read::Here, documents, person, kind)
    }

    /// The document `route` names, kept in `documents`, changed by `person`, on this device alone: a
    /// window run without a session, which has no connection to a workspace. It is the on-disk
    /// copy, which this device governs; its changes are kept there and go nowhere else.
    ///
    /// # Errors
    /// The route names no document, or its copy cannot be opened.
    pub fn open_local(
        route: RouteSpec,
        documents: &Rc<dyn DurableStore>,
        person: impl Into<String>,
        kind: impl Into<String>,
    ) -> Result<Self, DocumentError> {
        let (doc, writer) = open_stored(route_document(documents, &route)?);
        let (chunks, writers) = held_chunks(documents, &route, doc, writer);
        let mut record = Self::holding(
            chunks,
            Some(writers),
            CollaborationManager::disabled(),
            route,
            kind.into(),
            person.into(),
        );
        record.local = true;
        Ok(record)
    }

    /// [`Self::open`], with the broker's committed read taken off this thread: `read` is what
    /// [`PrimaryRead::of`](collaboration::native_initial::PrimaryRead::of) said the manager's
    /// route needs, and what it read. An owner that runs other instances meanwhile waits for
    /// the read on another thread and opens the document with it here.
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
        kind: impl Into<String>,
    ) -> Result<Self, DocumentError> {
        Self::open_with(
            manager,
            Read::Taken(read.map(Box::new)),
            documents,
            person,
            kind,
        )
    }

    fn open_with(
        manager: CollaborationManager,
        read: Read,
        documents: &Rc<dyn DurableStore>,
        person: impl Into<String>,
        kind: impl Into<String>,
    ) -> Result<Self, DocumentError> {
        use collaboration::document::save::{
            open_stored_adopting, open_with_committed_read, workspace_document,
        };
        use collaboration::native_initial::{self, NativeConstruction};
        let person = person.into();
        let route = manager
            .primary_route()
            .map(|joined| joined.route().clone())
            .ok_or_else(|| DocumentError::Open("the connection joined no route".to_owned()))?;
        let workspace = route.cluster_id.to_string();
        let store = route_document(documents, &route)?;
        // Construction ownership reads the committed history before the document is built, as a
        // native launch's does: only a route whose reads are committed has one.
        let construction = match read {
            Read::Taken(read) => NativeConstruction::enter_read(manager, read.map(|read| *read)),
            Read::Here => NativeConstruction::enter(manager),
        }
        .map_err(DocumentError::Open)?;
        let primary = native_initial::primary(&route.app_id, Some(&workspace));
        let governs = primary.is_none();
        // A document read from the workspace is the workspace's up to what was read. One opened from this
        // replica's own copy was handed to the workspace by nothing yet: its first changes (the
        // copy's creation among them) are made here, and a sealed workspace's channel holds only
        // what was written on it, so the first send hands it all of them.
        let handed_nothing = primary.is_none();
        let opened = match primary {
            Some(read) => read.and_then(|(_, read)| open_with_committed_read(store, &read)),
            // A member's own document of an app that kept the workspace's document before starts
            // from what that document held on this device (`open_stored_adopting`).
            None => Ok(match route.document.as_deref() {
                Some(record) if !record.is_empty() && crate::keeps_own_resources(&route.app_id) => {
                    open_stored_adopting(
                        store,
                        workspace_document(documents, &route.app_id, &workspace).ok(),
                    )
                }
                _ => open_stored(store),
            }),
        };
        let (mut doc, mut writer) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                native_initial::fail(&error);
                return Err(DocumentError::Open(error));
            }
        };
        let manager = native_initial::transfer_to_app(native_initial::take_manager)
            .map_err(DocumentError::Open)?
            .ok_or_else(|| DocumentError::Open("the connection was not handed back".to_owned()))?;
        drop(construction);
        let previous = if doc.get_heads().is_empty() {
            previous_profile_document(documents, &route, &person)?
        } else {
            None
        };
        let restored = previous.is_some();
        let mut earlier_chunks = Vec::new();
        if let Some((mut copy, parts)) = previous {
            copy.set_actor(doc.get_actor().clone());
            writer.request(&copy);
            doc = copy;
            earlier_chunks = parts;
        }
        let (mut chunks, mut writers) = held_chunks(documents, &route, doc, writer);
        for (index, copy) in earlier_chunks {
            if let Some(writer) = writers.start(&route, index) {
                writer.request(&copy);
            }
            chunks.insert(index, copy);
        }
        let mut record = Self::holding(chunks, Some(writers), manager, route, kind.into(), person);
        record.governs = governs;
        if handed_nothing || restored {
            record.sent.clear();
        }
        // The chunks the document's manifest names are the ones this replica reads: it opens them
        // all, so a change of one reaches it ([`Self::join_chunks`]).
        record.join_chunks();
        Ok(record)
    }

    /// The document as this replica holds it: chunk [`FIRST`], which every document has and which
    /// holds the manifest of where everything else is. Its committed changes only: the store's
    /// unsaved writes are no change of the document, and a copy that kept them would commit them
    /// as one when it is saved.
    #[must_use]
    pub fn doc(&self) -> Doc {
        self.chunk(FIRST).unwrap_or_default()
    }

    /// A copy of the document and the chunks above it this replica holds, their committed changes
    /// only ([`Self::doc`]): to look at, as a test does. A frame reads through [`Self::read`].
    #[must_use]
    pub fn chunks(&self) -> Chunks {
        let mut copy = Chunks::of(self.doc());
        self.read(|chunks| {
            for (chunk, _) in chunks.all().filter(|(chunk, _)| *chunk != FIRST) {
                copy.insert(chunk, chunks.committed(chunk).unwrap_or_default());
            }
        });
        copy
    }

    /// A copy of the chunk `chunk`, its committed changes only ([`Self::doc`]).
    #[must_use]
    pub fn chunk(&self, chunk: ChunkNo) -> Option<Doc> {
        self.read(|chunks| chunks.committed(chunk))
    }

    /// Read the document and the chunks above it this replica holds.
    pub fn read<R>(&self, read: impl FnOnce(&Chunks) -> R) -> R {
        self.chunks.read(read)
    }

    /// Bring `instance` up to date with the document: the first time, its store opens over the
    /// document's chunks and holds them from then on; after that, what the document's writes moved
    /// is already in the store, and the instance's frame reads it.
    pub fn sync_instance(&mut self, instance: &mut Instance) {
        let moved = std::mem::take(&mut self.moved);
        if let Docs::Own(chunks) = &mut self.chunks {
            if let Some(chunks) = instance.open_over(std::mem::take(chunks)) {
                // No store opened over them: the document keeps them, and the next frame tries
                // again.
                self.chunks = Docs::Own(chunks);
                self.moved = true;
                return;
            }
            if let Some(host) = instance.host() {
                self.held_by(host.store());
            }
            return;
        }
        instance.sync_held(moved);
        // A host started over on a new build took the chunks over from the last one.
        if let Some(host) = instance.host() {
            self.held_by(host.store());
        }
    }

    /// The person at the window, as the runtime names them.
    fn person_actor(&self) -> crate::runtime::Actor {
        crate::runtime::Actor::Member(crate::runtime::ids::MemberId::person(self.person.clone()))
    }

    /// The steps that are the person's own: what their undo and redo take.
    fn own_steps(&self) -> Scope {
        Scope::Invoker(self.person_actor())
    }

    /// Read and write the chunks through `store` from here on, which writes as the person at
    /// the window: what they do there is written once, as the change it makes.
    fn held_by(&mut self, store: &crate::store::TiloStore) {
        store.set_author(&self.person_actor());
        self.chunks = Docs::Store(store.clone());
    }

    /// Have `host`'s store hold the document's chunks from here on, the one copy of each, which
    /// the document reads and writes through it: what a host with no window over the document (the
    /// document's origin applying what others ask) opens with.
    pub fn hold_in(&mut self, host: &crate::host::Host) {
        if let Docs::Own(chunks) = &mut self.chunks {
            host.adopt(std::mem::take(chunks));
            self.held_by(host.store());
        }
    }

    /// The collab client the document travels through.
    #[must_use]
    pub fn manager(&self) -> &CollaborationManager {
        &self.manager
    }

    /// The collab client the document travels through, to send what else the instance says on
    /// its connection (actions, presence).
    pub fn manager_mut(&mut self) -> &mut CollaborationManager {
        &mut self.manager
    }

    /// The route the document travels on.
    #[must_use]
    pub fn route(&self) -> &RouteSpec {
        &self.route
    }

    /// Whether the document's changes are stored: in memory only without an on-disk copy. Every
    /// chunk is stored to its own copy, so the document is stored when all of them are.
    #[must_use]
    pub fn save_status(&self) -> ResourceSaveStatus {
        let Some(writers) = &self.writers else {
            return ResourceSaveStatus::MemoryOnly;
        };
        let mut status = ResourceSaveStatus::Saved;
        for (chunk, writer) in &writers.chunks {
            let Some(of) = self.read(|chunks| {
                chunks
                    .held(*chunk)
                    .map(|doc| ResourceSaveStatus::of_writer(doc, writer))
            }) else {
                continue;
            };
            match of {
                failure @ ResourceSaveStatus::Failed(_) => return failure,
                ResourceSaveStatus::Saving => status = ResourceSaveStatus::Saving,
                ResourceSaveStatus::Saved | ResourceSaveStatus::MemoryOnly => {}
            }
        }
        status
    }

    /// Whether the document holds changes the workspace was not handed yet: they go with the next
    /// send ([`Self::share`]).
    #[must_use]
    pub fn unsent(&mut self) -> bool {
        !self.unsent_chunks().is_empty()
    }

    /// The chunks holding changes the workspace was not handed yet, the document first.
    fn unsent_chunks(&self) -> Vec<ChunkNo> {
        self.read(|chunks| {
            chunks
                .all()
                .filter(|(chunk, doc)| {
                    !heads_equal(
                        &doc.committed_heads(),
                        self.sent.get(chunk).map_or(&[], Vec::as_slice),
                    )
                })
                .map(|(chunk, _)| chunk)
                .collect()
        })
    }

    /// Why the document's changes stay on this device, when the collab client refused them for
    /// good: they are kept, in the document and on disk, but no longer shared.
    #[must_use]
    pub fn unshared(&self) -> Option<&str> {
        self.unshared.as_deref()
    }

    /// Write what the document holds to its on-disk copies now, as a window does before it closes:
    /// every chunk it holds, each to its own.
    pub fn flush(&mut self) {
        let Self {
            writers, chunks, ..
        } = self;
        let Some(writers) = writers.as_mut() else {
            return;
        };
        for (chunk, writer) in &mut writers.chunks {
            chunks.read(|chunks| {
                if let Some(doc) = chunks.held(*chunk) {
                    writer.request(doc);
                }
            });
            writer.flush();
        }
    }

    /// Have the on-disk copy of chunk `chunk` keep what it holds: the chunk's change goes in the
    /// same durable step as the commit that made it, as the document's own does.
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
        chunks.read(|chunks| {
            if let Some(doc) = chunks.held(chunk)
                && let Some(writer) = writers.start(route, chunk)
            {
                writer.request(doc);
            }
        });
    }

    /// The chunk `chunk`, to write into: the one this replica holds, or the one it starts here
    /// (a write into a run that begins there, before that chunk arrives), with the writer that
    /// keeps it on disk like the document's own.
    fn write_chunk<R>(&mut self, chunk: ChunkNo, write: impl FnOnce(&mut Doc) -> R) -> R {
        self.keep_on_disk(chunk);
        let (out, moved) = self.chunks.write_chunk(chunk, write);
        self.moved |= moved;
        out
    }

    /// Start the on-disk copy of chunk `chunk`, before it is written: a chunk this replica
    /// writes to keeps its own copy, so the change goes to disk with the document's own.
    fn keep_on_disk(&mut self, chunk: ChunkNo) {
        let Self { writers, route, .. } = self;
        if let Some(writers) = writers.as_mut()
            && !writers.chunks.contains_key(&chunk)
        {
            let _ = writers.start(route, chunk);
        }
    }

    /// The heads chunk `chunk` holds: none when this replica holds no such chunk.
    fn heads_of(&self, chunk: ChunkNo) -> Vec<ChangeHash> {
        self.read(|chunks| {
            chunks
                .held(chunk)
                .map(Doc::committed_heads)
                .unwrap_or_default()
        })
    }

    /// Whether the person at the window has a step to undo, or an undo to redo.
    #[must_use]
    pub fn can_undo(&self) -> (bool, bool) {
        self.can_undo_as(&self.person_actor())
    }

    /// Whether `who` has a step to undo, or an undo to redo, in the document: what the history
    /// verbs `undo` and `redo` would take for them.
    #[must_use]
    pub fn can_undo_as(&self, who: &crate::runtime::Actor) -> (bool, bool) {
        let scope = Scope::Invoker(who.clone());
        let owns = |version: &tilo_resource::history::Version| scope.owns(version);
        let mut undo = false;
        let mut redo = false;
        self.read(|chunks| {
            for (_, doc) in chunks.all() {
                undo |= !steps::undo_preview(doc, &scope, 1).is_empty();
                redo |= !steps::undos_in_effect(doc, &owns, 1).is_empty();
            }
        });
        (undo, redo)
    }

    /// The invocation the person's last step in the document ran in: the id every change of the
    /// action that made it names, in this unit and in every other one the action wrote.
    #[must_use]
    pub fn last_step_invocation(&self) -> Option<InvocationId> {
        let scope = self.own_steps();
        let step = self.last_step(&scope)?;
        self.invocation_of(&step)
    }

    /// The newest step `scope` has in effect among the chunks: the change whose time is the
    /// latest, the document's own before a chunk's when two share one (a change's time is what
    /// orders the chunks, which share no history between them).
    fn last_step(&self, scope: &Scope) -> Option<[u8; 32]> {
        self.read(|chunks| {
            chunks
                .all()
                .filter_map(|(_, doc)| {
                    let step = steps::undo_preview(doc, scope, 1).into_iter().next()?;
                    Some((step.time_ms, step.hash.0))
                })
                .max()
                .map(|(_, hash)| hash)
        })
    }

    /// The changes of this unit the invocation `invocation` made, oldest first: the unit's part
    /// of that action, read from the documents themselves. An undo across the action needs
    /// nothing else, so it works after a restart that lost every log the transport kept.
    #[must_use]
    pub fn changes_of(&self, invocation: InvocationId) -> Vec<[u8; 32]> {
        let mut changes: Vec<(i64, [u8; 32])> = Vec::new();
        self.read(|chunks| {
            // What each change says of itself is enough: its operations are never read.
            for (_, doc) in chunks.all() {
                for change in doc.committed_document().get_changes_meta(&[]) {
                    if change
                        .message
                        .as_deref()
                        .and_then(ActionContext::from_change_message)
                        .is_some_and(|context| context.source_invocation == Some(invocation))
                    {
                        changes.push((change.timestamp, change.hash.0));
                    }
                }
            }
        });
        changes.sort_unstable();
        changes.into_iter().map(|(_, hash)| hash).collect()
    }

    /// The chunk of this instance that holds the change `change`.
    #[must_use]
    pub fn chunk_of_change(&self, change: &[u8; 32]) -> Option<ChunkNo> {
        self.read(|chunks| {
            chunks
                .all()
                .find(|(_, doc)| {
                    doc.committed_document()
                        .get_change_meta_by_hash(&ChangeHash(*change))
                        .is_some()
                })
                .map(|(chunk, _)| chunk)
        })
    }

    /// The invocation the change `change` of this instance ran in.
    fn invocation_of(&self, change: &[u8; 32]) -> Option<InvocationId> {
        let chunk = self.chunk_of_change(change)?;
        self.read(|chunks| {
            chunks
                .held(chunk)?
                .committed_document()
                .get_change_meta_by_hash(&ChangeHash(*change))?
                .message
                .as_deref()
                .and_then(ActionContext::from_change_message)
                .and_then(|context| context.source_invocation)
        })
    }

    /// Whether the step the change `change` is part of is still in effect for the person: what an
    /// undo of it takes back, and not a step already undone or not theirs.
    #[must_use]
    pub fn can_revert(&self, change: &[u8; 32]) -> bool {
        let Some(chunk) = self.chunk_of_change(change) else {
            return false;
        };
        let scope = self.own_steps();
        let change = ChangeHash(*change);
        self.read(|chunks| {
            chunks.held(chunk).is_some_and(|doc| {
                steps::step_in_effect(doc, &|version| scope.owns(version), &change).is_some()
            })
        })
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
    ) -> Result<(), DocumentError> {
        let edit = HistoryEdit::Undo {
            steps: None,
            invocation: None,
            by: None,
            to: None,
            step: Some(ChangeHash(*change).to_string()),
        };
        let chunk = self.chunk_of_change(change).unwrap_or(FIRST);
        self.write_history(chunk, &edit, resource, kind, invoker, Some(ambient))?;
        self.sync_instance(instance);
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
    ) -> Result<bool, DocumentError> {
        let scope = self.own_steps();
        let t_last = std::time::Instant::now();
        let Some(step) = self.last_step(&scope) else {
            return Ok(false);
        };
        eprintln!("DBG undo last_step {:?}", t_last.elapsed());
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
            let invoker = ActionInvoker::person(self.person.clone());
            let chunk = self.chunk_of_change(&change).unwrap_or(FIRST);
            let edit = HistoryEdit::Undo {
                steps: None,
                invocation: None,
                by: None,
                to: None,
                step: Some(ChangeHash(change).to_string()),
            };
            self.write_history(chunk, &edit, resource, kind, invoker, Some(&ambient))?;
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
    ) -> Result<bool, DocumentError> {
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
                    DocumentError::Refused(format!(
                        "{change} names no change of the document: {error}"
                    ))
                })?;
                self.chunk_of_change(&change.0).unwrap_or(FIRST)
            }
            HistoryAsk::Undo | HistoryAsk::Redo | HistoryAsk::UndoRun(_) => FIRST,
        };
        let me = ActionInvoker::person(self.person.clone());
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
    ) -> Result<bool, DocumentError> {
        if !self.can_undo().1 {
            return Ok(false);
        }
        let scope = self.own_steps();
        let owns = |version: &tilo_resource::history::Version| scope.owns(version);
        let ambient = ActionContext {
            source_invocation: Some(InvocationId::new()),
            ..ActionContext::default()
        };
        // The undo being redone may have taken back changes in several chunks: each is brought
        // back in the chunk that holds it, as one action, sent once all of them are.
        let undone: Vec<ChunkNo> = self.read(|chunks| {
            chunks
                .all()
                .filter(|(_, doc)| !steps::undos_in_effect(doc, &owns, 1).is_empty())
                .map(|(chunk, _)| chunk)
                .collect()
        });
        let mut brought = false;
        for chunk in undone {
            let edit = HistoryEdit::Redo { steps: None };
            let invoker = ActionInvoker::person(self.person.clone());
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
        let t = std::time::Instant::now();
        self.share_into(instance);
        eprintln!("DBG finish_history took {:?}", t.elapsed());
        true
    }

    /// Apply the history verb `edit`, invoked on `resource` (a resource `kind`) by `invoker` in
    /// `ambient`, to the document as one step of its own, as a native app applies it; send it on
    /// the document's route and bring the instance up to date. What the verb did, as its caller
    /// is answered ([`verbs::result`]).
    pub fn history(
        &mut self,
        instance: &mut Instance,
        edit: &HistoryEdit,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Option<Json>, DocumentError> {
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
    ) -> Result<Option<Json>, DocumentError> {
        let (result, unshared) =
            self.history_step(chunk, edit, resource, kind, invoker, ambient)?;
        if let Some(why) = unshared {
            instance.failed(why);
        }
        self.sync_instance(instance);
        Ok(result)
    }

    /// [`Self::history`] with no instance: the document's origin applying an agent task's history
    /// verb (its `undo`).
    pub fn history_at_origin(
        &mut self,
        edit: &HistoryEdit,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Option<Json>, DocumentError> {
        match self.history_step(FIRST, edit, resource, kind, invoker, ambient)? {
            (_, Some(why)) => Err(DocumentError::Refused(why)),
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
    ) -> Result<(Option<Json>, Option<String>), DocumentError> {
        let result = self.history_apply(chunk, edit, resource, kind, invoker, ambient)?;
        Ok((result, self.share()))
    }

    /// Apply the history verb `edit` as one step, leaving it unsent, and say which heads it was
    /// applied on: what a caller that answers nobody needs ([`Self::history_apply`], which also
    /// reads what the verb did for its answer).
    fn write_history(
        &mut self,
        chunk: ChunkNo,
        edit: &HistoryEdit,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Vec<automerge::ChangeHash>, DocumentError> {
        let action = HostHistoryEdit::new(edit.clone(), self.words.clone());
        let encoded = action.encoded().map_err(DocumentError::Refused)?;
        // The verb's change is a step of its own, or a part of the one action an undo or redo
        // across chunks is (`ambient`'s invocation).
        let invocation = ambient
            .and_then(|ambient| ambient.source_invocation)
            .unwrap_or_default();
        let context = ActionContext::for_resource(resource, kind, kind)
            .with_source_invocation(Some(invocation));
        self.apply_on(chunk, (&action, None), encoded, context, invoker, ambient)
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
    ) -> Result<Option<Json>, DocumentError> {
        let before = self.write_history(chunk, edit, resource, kind, invoker, ambient)?;
        let words = &self.words;
        self.chunks.read(|chunks| {
            let Some(doc) = chunks.held(chunk) else {
                return Err(DocumentError::Refused(format!(
                    "chunk {chunk} is not held, though its change was taken"
                )));
            };
            let patches = doc.committed_diff(&before, &doc.committed_heads());
            Ok(verbs::result(edit.verb(), doc, &patches, words))
        })
    }

    /// Answer the history view `payload` asks ([`verbs::VIEW_TYPE`]) of the document.
    ///
    /// The view is the document's own chunk: a step written in another chunk is listed there. The
    /// action that made it is in the workspace's log, which names every unit the action wrote.
    pub fn view_history(&self, payload: &[u8]) -> Result<tilo_actions::ActionOutput, String> {
        self.read(|chunks| {
            let doc = chunks.first().ok_or("the document is not held here")?;
            verbs::answer(doc, payload, &self.words)
        })
    }

    /// [`Self::share`], said in `instance` when the workspace refused it for good, and bring
    /// `instance` up to date.
    fn share_into(&mut self, instance: &mut Instance) {
        if instance.take_save_again()
            && let Some(handed) = &mut self.refused
        {
            handed.clear();
        }
        if let Some(why) = self.share() {
            instance.failed(why);
        }
        self.sync_instance(instance);
    }

    /// Hand the workspace what the document holds past what it was handed, as one change. What the
    /// collab client cannot take yet (its queue is full, or the route is still connecting) stays
    /// in the document and goes with the next send: a frame sends again while any is left
    /// ([`Self::unsent`]), and the client wakes its owner once it has workspace. A change it
    /// refuses for good (above its size limit) stays on this device, kept in the document and on
    /// disk: the reason is returned the first time, for the window to say so.
    pub fn share(&mut self) -> Option<String> {
        self.send(None)
    }

    /// What this unit holds past what the workspace was handed, as its parts: one per chunk,
    /// each the part of an action its caller sends with every other unit of the action
    /// ([`Self::share_under`]). Empty when the workspace holds all of it.
    ///
    /// A chunk the workspace was handed nothing of goes whole (a snapshot): the workspace founds a
    /// record it holds nothing of only from the whole of it, and a change alone builds on nothing
    /// there.
    pub fn unsent_parts(&self) -> Vec<(ChunkNo, DocumentMutation)> {
        self.read(|chunks| {
            chunks
                .all()
                .filter_map(|(chunk, doc)| {
                    let sent = self.sent.get(&chunk).map_or(&[][..], Vec::as_slice);
                    if sent.is_empty() {
                        return (!doc.committed_heads().is_empty())
                            .then(|| (chunk, DocumentMutation::Snapshot(doc.save_committed())));
                    }
                    let change = doc.committed_document().save_after(sent);
                    (!change.is_empty()).then_some((chunk, DocumentMutation::Incremental(change)))
                })
                .collect()
        })
    }

    /// [`Self::share`] as this unit's part of `commit`, the one action across every unit it
    /// wrote: the parts travel together, each carrying the same commit, so a peer applies all of
    /// the action or none of it.
    pub fn share_under(&mut self, commit: &collab_types::WorkspaceCommit) -> Option<String> {
        self.send(Some(commit))
    }

    fn send(&mut self, commit: Option<&collab_types::WorkspaceCommit>) -> Option<String> {
        if self.local {
            // No workspace to hand them to: what is kept on disk is all there is.
            for chunk in self.unsent_chunks() {
                self.keep(chunk);
                self.note_sent(chunk);
            }
            return None;
        }
        // What the changes name as content goes first, so the document's host holds it when a
        // reader asks for it.
        self.send_content();
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
        // While the workspace refuses, nothing goes again until the document moved on or the
        // person asked to try again.
        let heads = self.refused.is_some().then(|| self.marks());
        if heads.is_some() && heads == self.refused {
            return None;
        }
        let mut handed = true;
        let parts = self.unsent_parts();
        // The parts of one action that wrote several chunks go out as one workspace commit,
        // naming every one of them: a peer applies all of the action or none of it. A part whose
        // action wrote one chunk travels as any change does.
        let commits = match commit {
            Some(commit) => vec![Some(commit.clone()); parts.len()],
            None => self.commits_of(&parts),
        };
        for ((chunk, mutation), commit) in parts.into_iter().zip(commits) {
            let request = match (mutation, commit) {
                // The chunk's first write founds it: the whole of it, which no commit names.
                (DocumentMutation::Snapshot(whole), _) => CollabRequest::document_snapshot(whole),
                (DocumentMutation::Incremental(change), _) if change.is_empty() => {
                    // The workspace holds all of it already, by heads named apart.
                    self.note_sent(chunk);
                    continue;
                }
                (change, Some(commit)) => CollabRequest::document_action(change, commit),
                (DocumentMutation::Incremental(change), None) => {
                    CollabRequest::document_incremental(change)
                }
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
            handed &= sent;
            // What goes while the workspace refuses is not known to be taken: the document goes
            // whole until it is.
            if sent && self.refused.is_none() {
                self.note_sent(chunk);
            }
        }
        if handed && heads.is_some() {
            self.refused = heads;
        }
        None
    }

    /// Follow whether the workspace refuses the document's changes, as its connection says: whether
    /// it now does, when that changed.
    fn follow_refusal(&mut self) -> Option<bool> {
        match (self.manager.refused_change(), &self.refused) {
            (Some(why), None) => {
                tracing::warn!(
                    app = %self.route.app_id,
                    %why,
                    "workspace: it refused a change; the document's changes wait on this device"
                );
                self.sent.clear();
                self.refused = Some(self.marks());
                Some(true)
            }
            // A change went through since: what the workspace was handed whole is its own now.
            (None, Some(_)) => {
                if let Some(handed) = self.refused.take() {
                    self.sent = handed;
                }
                Some(false)
            }
            _ => None,
        }
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
            .map(|(chunk, _)| self.chunk_document(*chunk))
            .collect();
        let units: Vec<collab_types::UnitMutation<'_>> = parts
            .iter()
            .zip(&records)
            .map(|((_, mutation), record)| collab_types::UnitMutation {
                app: &self.route.app_id,
                document_id: record,
                mutation,
                document: None,
            })
            .collect();
        let author = collab_types::CommitAuthor {
            actor: None,
            device: [0; 32],
        };
        collab_types::WorkspaceCommit::of_units(self.route.cluster_id, &author, &units).map_or_else(
            |_| vec![None; parts.len()],
            |commits| {
                commits
                    .into_iter()
                    .map(|commit| commit.filter(|commit| commit.parts.len() > 1))
                    .collect()
            },
        )
    }

    /// The document id chunk `chunk` travels on: the document's own for [`FIRST`], the chunk's
    /// document's above it ([`chunk::document_id`]).
    fn chunk_document(&self, chunk: ChunkNo) -> String {
        let base = self.route.document.as_deref().unwrap_or("");
        if chunk == FIRST {
            base.to_owned()
        } else {
            chunk::document_id(base, chunk)
        }
    }

    /// The routes this replica takes changes in on: the document's own, and each chunk's it holds
    /// or the manifest names ([`Self::chunk_route`]).
    fn taken_routes(&self) -> Vec<(RouteSpec, ChunkNo)> {
        let chunks: BTreeSet<ChunkNo> = self.read(|chunks| {
            let mut named = chunks.placement().chunks();
            named.extend(chunks.all().map(|(chunk, _)| chunk).filter(|c| *c != FIRST));
            named
        });
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
        let heads = self.heads_of(chunk);
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
        let heads = self.heads_of(chunk);
        self.sent.insert(chunk, heads);
    }

    /// Send `request` on the route of chunk `chunk`: the document's own route for chunk
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

    /// The route chunk `chunk` of the document travels on: the document's own for [`FIRST`], the
    /// chunk's document's above it ([`chunk::document_id`]).
    #[must_use]
    pub fn chunk_route(&self, chunk: ChunkNo) -> RouteSpec {
        chunk_route(&self.route, chunk)
    }

    /// Join the route of every chunk the document's manifest names: a chunk's changes reach only
    /// the replicas that opened it, so a chunk this replica holds or wants is one it joined.
    fn join_chunks(&mut self) {
        for chunk in self.read(|chunks| chunks.placement().chunks()) {
            if !self.joined.insert(chunk) {
                continue;
            }
            self.manager.join_route(
                self.chunk_route(chunk),
                collab_types::document::ResourceType::new(self.kind.clone()),
            );
        }
    }

    /// Hand the workspace the whole of chunk `chunk` on its route, as the answer to a late joiner
    /// the broker asked this replica to bring up to date. One the collab client does not take is
    /// said: the joiner asks again while no holder answers.
    fn send_snapshot(&mut self, chunk: ChunkNo) {
        let Some(whole) = self.read(|chunks| chunks.held(chunk).map(Doc::save_committed)) else {
            return;
        };
        let request = CollabRequest::document_snapshot(whole);
        if let Err(error) = self.send_on(chunk, request) {
            tracing::warn!(chunk, %error, "workspace: a snapshot for a late joiner was not sent");
        }
    }

    /// Commit every change `instance` queued for the workspace, in order, as a step each, and send
    /// them on the document's route ([`Self::share`]); the instance hears each answered. Returns
    /// the tickets committed.
    ///
    /// A change is the workspace's once it is in the document: the collab client not taking it yet
    /// never refuses it. An invocation of anything but the host's `commit` is answered refused:
    /// this path runs only what the instance planned itself.
    ///
    /// # Errors
    /// The first change that could not be applied to the document; it is answered refused, and
    /// what is still queued stays queued.
    pub fn commit(&mut self, instance: &mut Instance) -> Result<Vec<u64>, DocumentError> {
        let committed = self.commit_queued(instance);
        self.share_into(instance);
        committed
    }

    fn commit_queued(&mut self, instance: &mut Instance) -> Result<Vec<u64>, DocumentError> {
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

    fn commit_ticket(&mut self, instance: &Instance, ticket: u64) -> Result<(), DocumentError> {
        let host = instance.host().ok_or(DocumentError::NoHost)?;
        let action = host
            .store()
            .committed(ticket)
            .ok_or(DocumentError::NoAction(ticket))?;
        let invoker = ActionInvoker::person(self.person.clone());
        // The stage of the person's the change is one of, when it is one of a burst of typing;
        // every other change is a step of its own.
        let gesture = action
            .authorship
            .gesture
            .unwrap_or_else(tilo_actions::ActionId::new);
        self.apply_in_gesture(&action, &invoker, None, Some(gesture))?;
        Ok(())
    }

    /// Apply `action`, which the API built for `invoker` (a person or an agent task the broker
    /// admitted), to the workspace document as one step of its own, and send it on the document's route.
    /// `ambient` is the task or run it was invoked in. The instance is brought up to date.
    pub fn apply_invoked(
        &mut self,
        instance: &mut Instance,
        action: &DynAction,
        invoker: &ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(), DocumentError> {
        self.apply(action, invoker, ambient)?;
        self.share_into(instance);
        Ok(())
    }

    fn apply(
        &mut self,
        action: &DynAction,
        invoker: &ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(), DocumentError> {
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
    ) -> Result<(), DocumentError> {
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
    ) -> Result<(), DocumentError> {
        let parts = self.read(|chunks| chunks.parts_of(action));
        let mut wrote: Vec<(ChunkNo, Vec<ChangeHash>)> = Vec::new();
        for (chunk, part) in parts {
            let encoded = part
                .encode()
                .map_err(|error| DocumentError::Refused(error.to_string()))?;
            match self.apply_on(
                chunk,
                (&part, Some(&part)),
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
            // A run the action started, filled or moved is written in the document's manifest: the
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
            if let Err(error) = self.write_chunk(chunk, |doc| fork_back(doc, &before)) {
                tracing::error!(
                    app = %self.route.app_id,
                    chunk,
                    %error,
                    "workspace: an action it did not take could not be taken back"
                );
            }
        }
        self.chunks.reread_placement();
    }

    /// Apply `action`, encoded as `encoded`, to the chunk `chunk` as one step of its own,
    /// recorded with `context`; the heads the chunk held before it. The caller sends it on the
    /// chunk's route ([`Self::share`]).
    ///
    /// What the chunk holds past those heads is kept only if the workspace takes it, as every
    /// receiver decides its claims against the app's surface
    /// ([`collab_types::app_surface::refusal`]): a change the workspace refuses is taken out of
    /// the chunk again, with all of it, so it is never kept here, built on and written to disk
    /// while the workspace holds none of it.
    ///
    /// `part`, when `action` is the part of a tilolang action the instance's store wrote, is
    /// committed as the store wrote it when that is how replaying it writes it
    /// ([`crate::store::TiloStore::commit_action`]), rather than written a second time.
    fn apply_on(
        &mut self,
        chunk: ChunkNo,
        (action, part): (&dyn tilo_actions::Action, Option<&DynAction>),
        encoded: EncodedAction,
        mut context: ActionContext,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Vec<ChangeHash>, DocumentError> {
        let _steps = OwnSteps::enter();
        if let Some(ambient) = ambient {
            context = context.enriched_by(ambient);
        }
        let author = invoker.acting.by.clone();
        let context = context.with_invoker(invoker);
        let workspace = self.route.cluster_id.to_string();
        let app = self.route.app_id.clone();
        let document_id = self.chunk_document(chunk);
        let record = collab_types::WrittenDocument {
            workspace: &workspace,
            app: &app,
            document: &document_id,
        };
        self.keep_on_disk(chunk);
        // What the store wrote of the action is its change, as replaying the action writes it:
        // committed as the author's, never applied a second time. A document on this device alone
        // has no workspace whose receivers could refuse it: what is written is kept.
        let local = self.local;
        let check = |doc: &mut Doc, before| {
            if local {
                Ok((before, None))
            } else {
                kept_or_refused(&record, doc, before)
            }
        };
        let (before, refused) = match &self.chunks {
            Docs::Store(store) => {
                let (out, moved) =
                    store.commit_action(chunk, (action, part), encoded, &author, context, check);
                self.moved |= moved;
                out.map_err(|error| DocumentError::Refused(error.to_string()))??
            }
            Docs::Own(_) => self.write_chunk(chunk, |doc| {
                let before =
                    crate::store::apply_step(doc, action, encoded, &author.actor_bytes(), context)
                        .map_err(|error| DocumentError::Refused(error.to_string()))?;
                check(doc, before)
            })?,
        };
        if let Some(why) = refused {
            tracing::warn!(%app, chunk, %why, "workspace: a change the workspace refuses was not kept");
            return Err(DocumentError::Refused(format!(
                "the workspace refuses it: {why}"
            )));
        }
        Ok(before)
    }

    /// Take in what the document's route brought and bring `instance` up to date with the
    /// document. Whether the document changed.
    pub fn receive(&mut self, instance: &mut Instance) -> bool {
        self.receive_and(instance, |_| {})
    }

    /// [`Self::receive`], handing every event that is no change of the document (an invocation,
    /// presence, the route's state) to `other`, in the order they came.
    pub fn receive_and(&mut self, instance: &mut Instance, other: impl FnMut(CollabEvent)) -> bool {
        let changed = self.take_in(other);
        if let Some(refused) = self.follow_refusal() {
            instance.refused_by_workspace(refused);
        }
        if let Some(why) = self.unreadable.take() {
            instance.failed(why);
        }
        if changed {
            self.sync_instance(instance);
        }
        changed
    }

    /// The heads of every chunk this replica holds now: what [`Self::changes_since`] starts from.
    #[must_use]
    pub fn marks(&self) -> Stamps {
        self.read(|chunks| {
            chunks
                .all()
                .map(|(chunk, doc)| (chunk, doc.committed_heads()))
                .collect()
        })
    }

    /// What this replica holds past `marks`, as Automerge changes: what a change applied here
    /// wrote, in whichever chunk, for its invoker to read back without waiting on the workspace.
    #[must_use]
    pub fn changes_since(&self, marks: &Stamps) -> Vec<u8> {
        let mut changes = Vec::new();
        self.read(|chunks| {
            for (chunk, doc) in chunks.all() {
                let from = marks.get(&chunk).map_or(&[][..], Vec::as_slice);
                changes.extend_from_slice(&doc.committed_document().save_after(from));
            }
        });
        changes
    }

    /// Take in what the document's route brought, with no instance to bring up to date: a document
    /// held only to apply changes at its origin. Events that are no change of the document are
    /// dropped, and so is what a window would have said about them. Whether the document changed.
    pub fn take_in_document(&mut self) -> bool {
        let changed = self.take_in(|_| {});
        self.follow_refusal();
        self.unreadable = None;
        changed
    }

    /// Apply `action`, which the API built for `invoker` (an agent task the broker admitted),
    /// to the workspace document as one step of its own in `ambient`, and send it on the document's
    /// route, with no instance: the document's origin applying a change.
    pub fn apply_change(
        &mut self,
        action: &DynAction,
        invoker: &ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(), DocumentError> {
        self.apply(action, invoker, ambient)?;
        match self.share() {
            Some(why) => Err(DocumentError::Refused(why)),
            None => Ok(()),
        }
    }

    fn take_in(&mut self, mut other: impl FnMut(CollabEvent)) -> bool {
        let mut changed = false;
        let mut resource_changed = false;
        let mut routes = self.taken_routes();
        self.send_content();
        for event in self.manager.poll() {
            let (chunk, before, applied) = match event {
                CollabEvent::Document {
                    ref envelope,
                    ref decrypted_payload,
                    ..
                } => match self.take_document(&routes, envelope, decrypted_payload.as_deref()) {
                    Taken::Change(taken) => taken,
                    Taken::Nothing => continue,
                    // Another document this connection joined (another app's, which the
                    // instance reads): its reader takes it.
                    Taken::OtherRoute => {
                        other(event);
                        continue;
                    }
                },
                CollabEvent::CommittedDocument { route, read } => {
                    // What the workspace committed of a route this replica holds, read for it
                    // by the broker: another replica's changes, or the whole document once this one
                    // was handed nothing of it. It is merged with what this replica made
                    // meanwhile, as a change arriving on the route is, and acknowledged then.
                    let Some(&(_, chunk)) = routes.iter().find(|(held, _)| *held == route) else {
                        // Another document this connection joined: its reader takes it.
                        other(CollabEvent::CommittedDocument { route, read });
                        continue;
                    };
                    let before = self.heads_of(chunk);
                    let Some(applied) =
                        self.write_chunk(chunk, |doc| read.take_into(doc).map(usize::from))
                    else {
                        if read.was_refused() && read.is_whole() {
                            // The workspace's copy of the document cannot be read
                            // here. The reader goes on from the revision it read
                            // and the document keeps what this device holds; the
                            // window says once that it is the copy on this
                            // device the person is looking at.
                            self.unreadable = Some(
                                "The shared copy of this document couldn't be read here. \
                                 Your changes stay on this device."
                                    .to_owned(),
                            );
                        }
                        continue;
                    };
                    (chunk, before, applied)
                }
                // Content the document names arrived, or where a want of it stands: the document's
                // store keeps it on its next read.
                CollabEvent::DocumentContent { route, hash, bytes } if route == self.route => {
                    self.link.arrive(hash, bytes);
                    continue;
                }
                CollabEvent::DocumentContentState { route, hash, state } if route == self.route => {
                    self.link.stands(hash, state);
                    continue;
                }
                CollabEvent::DocumentSnapshotRequested { route } => {
                    // The broker chose this connection as the one that answers a late joiner of
                    // the document or of one of its chunks: nobody else does, so it is answered
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
            // What is taken in counts as a change only when the chunk gained one: a change this
            // replica made comes back on its own route (the broker hands every holder what a
            // unit sent), and the chunk already holds it. The chunk's readers are told a change
            // once it holds it, so a change it held already is told to none of them.
            changed |= applied > 0;
            self.note_arrived(chunk, &before);
            if chunk == FIRST && applied > 0 {
                resource_changed = true;
                // The document names which chunks its resources are in: a change of it may name
                // ones this replica holds none of yet.
                routes = self.taken_routes();
            }
        }
        if resource_changed {
            self.chunks.reread_placement();
            self.join_chunks();
        }
        changed
    }

    /// Take in a change that arrived on one of `routes` as the signed `envelope` the broker
    /// relays: the chunk it was of, its heads before, and how many changes it gained.
    fn take_document(
        &mut self,
        routes: &[(RouteSpec, ChunkNo)],
        envelope: &collab_types::SignedOp,
        decrypted_payload: Option<&[u8]>,
    ) -> Taken {
        // The envelope names the route it was sealed for: the document's, or the route of the
        // chunk its content belongs to.
        for (route, chunk) in routes {
            let Some(payload) = validated_route_payload_with(route, envelope, decrypted_payload)
            else {
                continue;
            };
            let mutation = match decode_routed_mutation(payload, decrypted_payload) {
                Ok(mutation) => mutation,
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        "workspace: a change that does not decode was not taken in"
                    );
                    return Taken::Nothing;
                }
            };
            let before = self.heads_of(*chunk);
            return match self.write_chunk(*chunk, |doc| load_mutation(doc, &mutation)) {
                Ok(applied) => Taken::Change((*chunk, before, applied)),
                Err(error) => {
                    tracing::warn!(%error, chunk, "workspace: a change was not taken in");
                    Taken::Nothing
                }
            };
        }
        Taken::OtherRoute
    }
}

/// Why the workspace refuses what `doc`, the chunk `document`, holds past `before`, when it does.
fn refusal_of(
    record: &collab_types::WrittenDocument<'_>,
    doc: &Doc,
    before: &[ChangeHash],
) -> Option<String> {
    let change = doc.committed_document().save_after(before);
    if change.is_empty() {
        return None;
    }
    collab_types::app_surface::refusal(record, &collab_types::DocumentMutation::Incremental(change))
}

/// Take `doc` back to the heads `before`, writing on under the actor it had.
fn fork_back(doc: &mut Doc, before: &[ChangeHash]) -> Result<(), automerge::AutomergeError> {
    let actor = doc.get_actor().clone();
    let mut taken = doc.committed_fork_at(before)?;
    taken.set_actor(actor);
    *doc = taken;
    Ok(())
}

/// What `doc`, the chunk `document`, holds past `before` once an action wrote it: kept, with the
/// heads before it, or taken out again, with why the workspace refuses it.
fn kept_or_refused(
    record: &collab_types::WrittenDocument<'_>,
    doc: &mut Doc,
    before: Vec<ChangeHash>,
) -> Result<(Vec<ChangeHash>, Option<String>), DocumentError> {
    let Some(why) = refusal_of(record, doc, &before) else {
        return Ok((before, None));
    };
    fork_back(doc, &before).map_err(|error| {
        DocumentError::Refused(format!("{why}; and it could not be taken out: {error}"))
    })?;
    Ok((before, Some(why)))
}

/// The route chunk `chunk` of the document on `route` travels on: `route` itself for [`FIRST`], the
/// chunk's document's above it ([`chunk::document_id`]).
fn chunk_route(route: &RouteSpec, chunk: ChunkNo) -> RouteSpec {
    if chunk == FIRST {
        return route.clone();
    }
    RouteSpec::document(
        route.app_id.clone(),
        route.cluster_id,
        chunk::document_id(route.document.as_deref().unwrap_or(""), chunk),
    )
}

/// Another app's document an instance reads and never writes: the document of a package its app uses
/// that is installed as an app of its own (Today reading Tasks'). It is joined on the instance's
/// connection, read-only as the broker admits an app's declared read, with every chunk its
/// manifest names, and what the workspace committed of it is taken in as it arrives.
pub struct ReadDocument {
    route: RouteSpec,
    /// The kind of the document, as its app publishes it.
    kind: String,
    /// Its chunks: held here until the store of the app reading it holds them ([`Self::hold_in`]).
    chunks: Docs,
    joined: BTreeSet<ChunkNo>,
}

impl ReadDocument {
    /// The document on `route`, joined through `manager`: held empty until the workspace sends it.
    pub fn join(route: RouteSpec, manager: &mut CollaborationManager, kind: &str) -> Self {
        manager.join_route(
            route.clone(),
            collab_types::document::ResourceType::new(kind),
        );
        Self {
            route,
            kind: kind.to_owned(),
            chunks: Docs::Own(Chunks::of(Doc::new())),
            joined: BTreeSet::from([FIRST]),
        }
    }

    /// Have `host`'s store hold the document's chunks from here on, the one copy of each: what
    /// the document takes in is written into it, and derived there as it is.
    pub fn hold_in(&mut self, host: &crate::host::Host) {
        if let Docs::Own(chunks) = &mut self.chunks {
            host.adopt(std::mem::take(chunks));
            self.chunks = Docs::Store(host.store().clone());
        }
    }

    /// The route the document travels on.
    #[must_use]
    pub fn route(&self) -> &RouteSpec {
        &self.route
    }

    /// Take in `event` when it is a change of this document or one of its chunks, joining the
    /// chunks its manifest names from then on. `None` for an event of anything else; else
    /// whether the document gained a change.
    pub fn take(
        &mut self,
        event: &CollabEvent,
        manager: &mut CollaborationManager,
    ) -> Option<bool> {
        let routes: Vec<(RouteSpec, ChunkNo)> = self
            .joined
            .iter()
            .copied()
            .map(|chunk| (chunk_route(&self.route, chunk), chunk))
            .collect();
        let (chunk, applied) = match event {
            CollabEvent::Document {
                envelope,
                decrypted_payload,
                ..
            } => routes.iter().find_map(|(route, chunk)| {
                let payload = validated_route_payload_with(
                    route,
                    envelope,
                    decrypted_payload.as_deref(),
                )?;
                let applied = decode_routed_mutation(payload, decrypted_payload.as_deref())
                    .map_err(|error| format!("{error:?}"))
                    .and_then(|mutation| {
                        self.chunks
                            .write_chunk(*chunk, |doc| load_mutation(doc, &mutation))
                            .0
                            .map_err(|error| error.to_string())
                    })
                    .unwrap_or_else(|error| {
                        tracing::warn!(%error, app = route.app_id, "a read document's change was not taken in");
                        0
                    });
                Some((*chunk, applied))
            })?,
            CollabEvent::CommittedDocument { route, read } => {
                let &(_, chunk) = routes.iter().find(|(held, _)| held == route)?;
                let applied = self
                    .chunks
                    .write_chunk(chunk, |doc| read.take_into(doc).map(usize::from))
                    .0
                    .unwrap_or(0);
                (chunk, applied)
            }
            _ => return None,
        };
        if chunk == FIRST && applied > 0 {
            self.chunks.reread_placement();
            self.join_chunks(manager);
        }
        Some(applied > 0)
    }

    /// Join the route of every chunk the document's manifest names that is not joined yet.
    fn join_chunks(&mut self, manager: &mut CollaborationManager) {
        for chunk in self.chunks.read(|chunks| chunks.placement().chunks()) {
            if !self.joined.insert(chunk) {
                continue;
            }
            manager.join_route(
                chunk_route(&self.route, chunk),
                collab_types::document::ResourceType::new(self.kind.clone()),
            );
        }
    }
}

/// What became of a change that arrived on the connection.
enum Taken {
    /// It was of the chunk named, whose heads were these before, and it gained so many changes.
    Change((ChunkNo, Vec<ChangeHash>, usize)),
    /// It was of this document, and could not be taken in.
    Nothing,
    /// It was of no route of this document.
    OtherRoute,
}

/// Take `mutation` into `doc`, merged with what `doc` holds: the number of changes it gained.
fn load_mutation(
    doc: &mut Doc,
    mutation: &DocumentMutation,
) -> Result<usize, automerge::AutomergeError> {
    match mutation {
        DocumentMutation::Incremental(bytes) => doc.load_incoming(bytes),
        DocumentMutation::Snapshot(bytes) => doc.load_incoming_whole(bytes),
    }
}

/// Whether `a` and `b` name the same heads, in whatever order.
fn heads_equal(a: &[ChangeHash], b: &[ChangeHash]) -> bool {
    a.len() == b.len() && a.iter().all(|head| b.contains(head))
}

impl crate::instance::HistoryAsk {
    /// The history verb that carries it out on the document.
    #[must_use]
    pub fn edit(&self) -> HistoryEdit {
        let undo = |invocation: Option<&String>, step: Option<&String>| HistoryEdit::Undo {
            steps: None,
            invocation: invocation.cloned(),
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

/// While held, what is committed on this thread joins no undo step another history opened on it
/// (`tilo_actions::open_step`): a window's own document, where the window runs on the document's
/// thread. A change of the document is a step of its invocation's, or one of its own.
struct OwnSteps(Option<tilo_actions::ActionId>);

impl OwnSteps {
    fn enter() -> Self {
        let open = tilo_actions::current_step();
        tilo_actions::open_step(None);
        Self(open)
    }
}

impl Drop for OwnSteps {
    fn drop(&mut self) {
        tilo_actions::open_step(self.0);
    }
}

#[cfg(test)]
mod profile_tests {
    use automerge::{ReadDoc as _, transaction::Transactable as _};

    use super::*;

    #[test]
    fn previous_workspace_views_are_recovered_for_their_profile_only() {
        let documents: Rc<dyn DurableStore> = Rc::new(durable_store::MemoryStore::new());
        let app = "so.tilo.lang.views";
        let workspace = uuid::Uuid::from_u128(7);
        let ada = "ad".repeat(32);
        let bob = "b0".repeat(32);
        let old = RouteSpec::document(
            app,
            workspace,
            crate::member_document_of(app, &ada).unwrap(),
        );
        let mut store = route_document(&documents, &old).unwrap();
        let mut doc = store.new_document();
        doc.put(automerge::ROOT, "view", "my earlier view").unwrap();
        store.save(&mut doc).unwrap();
        let current = RouteSpec::document(
            app,
            collab_types::personal_cluster_of(&ada).unwrap(),
            collab_types::person_document_for_workspace("tl.so.tilo.lang.views", workspace),
        );
        assert!(
            previous_profile_document(&documents, &current, &bob)
                .unwrap()
                .is_none()
        );
        let (mut recovered, chunks) = previous_profile_document(&documents, &current, &ada)
            .unwrap()
            .unwrap();
        assert!(chunks.is_empty());
        assert_eq!(doc.get_heads(), recovered.get_heads());
        let (value, _) = recovered.get(automerge::ROOT, "view").unwrap().unwrap();
        assert_eq!(value.as_str(), Some("my earlier view"));
    }
}
