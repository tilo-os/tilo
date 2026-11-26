//! A workspace's resources, as the framework's one store keeps them ([`tilo_resource_store`]).
//!
//! Every resource of a workspace is in a document the store placed, one reader set to a
//! document, and a write of one is a member's signed write of that document, sealed for its
//! reader set like any write of an app's document. The host takes such a write into its store
//! ([`Store::receive`]), which decides it against the declared types and the writer's leave and
//! indexes what it changed in the same call: the store's index is the only one there is.
//!
//! The store keeps its documents in the workspace's own directory (`<workspace>/store`): each
//! document in `docs`, as it was saved whole with every change it took since appended, and the
//! index's snapshot of it with the heads it was taken at in `index`. A start reads the snapshot
//! of each document ([`Store::restore`]), and the document itself only when a write or a
//! member's change needs it; a document whose snapshot is missing or stale is read whole
//! ([`Store::load`]). A document nobody wrote for a while leaves memory ([`Store::evict`]), saved
//! whole with its snapshot, and so does every document when the service stops.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use automerge::{Automerge, Change as AmChange, ChangeHash, ROOT, ReadDoc as _};
use collab_types::is_store_document;
use tilo_resource_store::{Admit, Received, Refusal, Schema, Store};

use super::{HostedWorkspace, Workspaces};
use verb_grants::VerbGrants;

/// Why a command of this device's own roles on an app's record did not carry out
/// ([`Workspaces::record_command`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordFailure {
    /// The store or the workspace refused a write, saying why: what was written before it
    /// stands.
    Refused(String),
    /// The command could not be carried out at all.
    Failed(String),
}

/// Where a workspace's store keeps its documents, under the workspace's directory.
const STORE: &str = "store";
const DOCS: &str = "docs";
const INDEX: &str = "index";
/// How long a document nobody wrote stays in memory.
const IDLE: Duration = Duration::from_secs(300);

/// The longest name a file system gives one entry, less room for a partial write's suffix.
const NAME: usize = 240;

/// The file a document is kept in: its id, hex-encoded, since an id holds `/`. An id longer
/// than one name holds (a view its person alone reads names them, its kind and its window) is
/// kept below directories, each holding the next part of it and named with a `-`, which no
/// document's own name has.
fn file_name(doc: &str) -> PathBuf {
    let hex = hex::encode(doc.as_bytes());
    let mut parts: Vec<&str> = (0..hex.len())
        .step_by(NAME)
        .filter_map(|at| hex.get(at..hex.len().min(at.saturating_add(NAME))))
        .collect();
    let file = parts.pop().unwrap_or_default();
    let mut path: PathBuf = parts.iter().map(|part| format!("{part}-")).collect();
    path.push(file);
    path
}

/// The document the store under `root` placed as `doc`, saved as it holds it now: from memory,
/// or read from the file it kept when it left memory. `None` when the store placed none by that
/// id.
///
/// The store is read a command at a time under its own lock, never the workspace's
/// ([`super::readings::Reading`]): a read of a record answers while a write of the workspace
/// waits on the key service or a link.
pub(super) fn kept_document(
    store: &std::sync::Arc<std::sync::Mutex<Store>>,
    root: &Path,
    doc: &str,
) -> Option<Vec<u8>> {
    use tilo_lock::MutexRecover as _;
    let mut kept = store.lock_recover();
    if kept.document(doc).is_none() {
        let bytes = fs::read(root.join(DOCS).join(file_name(doc))).ok()?;
        kept.load(doc, &bytes).ok()?;
    }
    kept.document(doc).map(Automerge::save)
}

/// The store's content document of `app`'s record `record`, saved as the store holds it: what a
/// record's read answers, read a command at a time under the store's own lock and never the
/// workspace's ([`super::readings::Reading`]). `None` for a record the store holds nothing of.
pub(super) fn kept_record(
    store: &std::sync::Arc<std::sync::Mutex<Store>>,
    surfaces: &collab_types::app_surface::Surfaces,
    app: &str,
    record: &str,
) -> Option<Vec<u8>> {
    use tilo_lock::MutexRecover as _;
    let mut kept = store.lock_recover();
    let kind = record_changes::kind_of(surfaces, &kept, app, None, record)?;
    kept.content_document(&kind, record)
        .ok()
        .flatten()
        .map(Automerge::save)
}

/// Every document kept below `dir`, by its id, with its file: what [`file_name`] named.
fn kept_docs(dir: &Path, named: &str, docs: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if let Some(part) = name.strip_suffix('-') {
            kept_docs(&entry.path(), &format!("{named}{part}"), docs);
        } else if let Some(doc) = doc_of(&format!("{named}{name}")) {
            docs.push((doc, entry.path()));
        }
    }
}

/// The document a file keeps.
fn doc_of(file: &str) -> Option<String> {
    tilo_failure::ignored!(
        "a file not named for a document, as one an interrupted write left, is no document",
        String::from_utf8(hex::decode(file).ok()?).ok()
    )
}

/// Writes `bytes` to `path` whole or not at all.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = path.with_extension("partial");
    fs::write(&partial, bytes)?;
    fs::rename(&partial, path)
}

/// One workspace's store, and where it keeps its documents.
pub(super) struct Resources {
    /// Behind a lock of its own: the store reads documents through a source no two threads
    /// share. A reader holds it apart from the workspace's lock
    /// ([`super::readings::Reading`]), so what a record reads answers while a write of the
    /// workspace waits on the key service or a link.
    store: std::sync::Arc<std::sync::Mutex<Store>>,
    root: PathBuf,
    /// Each document in memory, by when it was last written.
    used: BTreeMap<String, Instant>,
    /// The documents whose kept snapshot is older than they are.
    unsnapshotted: BTreeSet<String>,
    /// How many surfaces the device had named when the store last declared them
    /// ([`collab_types::app_surface::Surfaces::registered`]).
    declared: u64,
    /// Whom a resource's people field names reads it as, by the workspace's policy as this
    /// device last read it: the store places reader sets by it.
    readers: Option<std::sync::Arc<PolicyReaders>>,
    /// The queries apps hold open, by the store's watch of their kind.
    opened: BTreeMap<u64, Opened>,
    /// The workspace, by its id spelled out.
    workspace: String,
    /// Where its live resources are shown, as the person who reads them here (their key in hex)
    /// may read them, once the service shows them anywhere ([`Self::show_live`]).
    live: Option<(crate::live::LiveRows, String)>,
    /// The store's [`Store::live_revision`] the rows were last shown at.
    live_shown: Option<u64>,
    /// The changes larger than one frame whose segments have not all come yet, by author.
    transfers: tilo_peer_state::document_frame::Transfers,
    /// The writes of actions across several documents that are not all here yet, by the action's
    /// commit id: the store takes an action's writes together, once every one of them is here.
    actions: BTreeMap<[u8; 32], Vec<ActionWrite>>,
    /// Whether a batch of writes is being kept ([`Self::batch`]), and whether one of them
    /// changed a document: its snapshot and the views of it are brought up to date once, as the
    /// batch ends.
    batch: Option<bool>,
}

/// One write of an action across several documents, held until the action's other writes are
/// here.
struct ActionWrite {
    author: [u8; 32],
    app: String,
    doc: String,
    envelope: collab_types::SignedOp,
    readers: String,
    changes: Vec<AmChange>,
}

/// The workspace's one read decision, for the store of the workspace: a person reads as
/// themselves, an agent as the person it acts for where its preset, narrowed to their reach,
/// reads the resource, and anyone else reads none of it.
struct PolicyReaders(std::sync::RwLock<std::sync::Arc<tilo_action_policy::WorkspacePolicy>>);

impl PolicyReaders {
    fn follow(&self, policy: std::sync::Arc<tilo_action_policy::WorkspacePolicy>) {
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = policy;
    }
}

impl tilo_resource_store::RecordReader for PolicyReaders {
    fn reader_of(
        &self,
        actor: &tilo_actions::Actor,
        kind: &str,
        id: &str,
    ) -> Option<tilo_action_policy::PersonId> {
        use tilo_action_policy::{
            Acting, ActionKind, Answer, Authority as Policy, Context, MemberId, TreePath,
        };
        let policy = self
            .0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match actor.member_id()? {
            MemberId::Person(person) => Some(person.clone()),
            MemberId::Agent { id: agent, person } => {
                let workspace = TreePath::workspace(&policy.workspace);
                // A tilolang record's kind names its app: `tl.<app>.<Type>`.
                let node = kind
                    .strip_prefix("tl.")
                    .and_then(|rest| rest.rsplit_once('.'))
                    .map_or_else(
                        || workspace.clone(),
                        |(app, _)| {
                            workspace
                                .clone()
                                .app(app)
                                .kind(kind)
                                .resource(id.to_owned())
                        },
                    );
                let acting = Acting::handed_agent(agent.as_str(), person.clone());
                let preset = acting.handed_preset(&policy.workspace);
                let reads = Policy::of_acting(&acting, &policy, &preset)
                    .decide(
                        &node,
                        ActionKind::Read,
                        &Context::at_time(tilo_clock::wall()),
                    )
                    .answer
                    == Answer::Allow;
                reads.then(|| person.clone())
            }
            MemberId::Bot(_) => None,
        }
    }
}

/// A query an app holds open: whenever a change makes what its page shows different, the page
/// is sent again, as the person it is read for may read it.
struct Opened {
    query: collab_types::store::Query,
    reader: String,
    pages: tokio::sync::mpsc::UnboundedSender<collab_types::store::Page>,
    shown: Vec<(String, collab_types::store::Indexed)>,
}

impl Resources {
    /// The store of the workspace whose directory is `workspace_dir`, started from what it kept
    /// there.
    pub(super) fn open(file: &Path, id: tilo_server_protocol::WorkspaceId) -> Self {
        let root = file
            .parent()
            .unwrap_or(file)
            .join(id.to_string())
            .join(STORE);
        let docs = root.join(DOCS);
        let reading = docs.clone();
        let mut store = Store::with_source(
            id.to_string(),
            Box::new(move |doc| fs::read(reading.join(file_name(doc))).ok()),
        );
        let mut used = BTreeMap::new();
        let mut unsnapshotted = BTreeSet::new();
        // What the store kept of each document is read side by side, and each document is
        // indexed from its snapshot: a workspace keeps a chain of parts, each with the
        // documents of its resources, and one read after another made a start wait for all
        // of them.
        let mut files = Vec::new();
        kept_docs(&docs, "", &mut files);
        let kept = read_snapshots(&root, &files);
        for ((doc, path), snapshot) in files.into_iter().zip(kept) {
            let restored =
                snapshot.is_some_and(|(heads, snapshot)| store.restore(&doc, &snapshot, &heads));
            if restored {
                continue;
            }
            // A document whose snapshot a killed write never left: it is read whole, and
            // kept with a snapshot for the next start.
            let loaded = fs::read(&path)
                .map_err(|error| error.to_string())
                .and_then(|bytes| store.load(&doc, &bytes).map_err(|error| error.to_string()));
            match loaded {
                Ok(()) => {
                    used.insert(doc.clone(), tilo_clock::now());
                    unsnapshotted.insert(doc);
                }
                Err(error) => {
                    tracing::warn!(%doc, %error, "a kept resource document does not read");
                }
            }
        }
        Self {
            store: std::sync::Arc::new(std::sync::Mutex::new(store)),
            root,
            used,
            unsnapshotted,
            declared: 0,
            readers: None,
            opened: BTreeMap::new(),
            workspace: id.to_string(),
            live: None,
            live_shown: None,
            transfers: tilo_peer_state::document_frame::Transfers::default(),
            actions: BTreeMap::new(),
            batch: None,
        }
    }

    /// Keep the writes until [`Self::batch_kept`] as one batch: each is in its document's file
    /// as it is taken, and the document's snapshot, the queries apps keep open and the live rows
    /// are brought up to date once, when the batch ends. A record's snapshot is the index of all
    /// it contains, so writing it for every write made an import of `n` writes take time in `n²`.
    pub(super) fn batch(&mut self) {
        self.batch.get_or_insert(false);
    }

    /// End the batch [`Self::batch`] began.
    pub(super) fn batch_kept(&mut self) {
        if self.batch.take() != Some(true) {
            return;
        }
        let docs: Vec<String> = self.unsnapshotted.iter().cloned().collect();
        for doc in docs {
            if let Err(error) = self.write_snapshot(&doc) {
                tracing::warn!(%doc, %error, "a resource document's snapshot was not kept");
            }
        }
        self.tell_opened();
        self.publish_live();
    }

    /// Show this workspace's live resources in `rows`, as `reader` (a person's key in hex) may read
    /// them, from now on.
    pub(super) fn show_live(&mut self, rows: &crate::live::LiveRows, reader: String) {
        if self.live.is_none() {
            self.live = Some((rows.clone(), reader));
            self.publish_live();
        }
    }

    /// Show the live resources again when the store's moved since they were last shown.
    fn publish_live(&mut self) {
        use tilo_lock::MutexRecover as _;
        let Some((rows, reader)) = self.live.clone() else {
            return;
        };
        let revision = self.store.lock_recover().live_revision();
        if self.live_shown == Some(revision) {
            return;
        }
        self.live_shown = Some(revision);
        let may_read = tilo_resource_store::wire::reads_as(&reader);
        let workspace = self.workspace.clone();
        let shown: Vec<crate::live::LiveRow> = self
            .store
            .lock_recover()
            .live(&may_read)
            .into_iter()
            .map(|live| crate::live::LiveRow {
                workspace: workspace.clone(),
                kind: live.kind,
                id: live.id,
                device: live.device,
                device_name: live.device_name,
                since: live.since,
            })
            .collect();
        rows.show(&workspace, shown);
    }

    /// The store, for the tests of a device's own writes to it.
    #[cfg(all(test, feature = "apps", feature = "agents"))]
    pub(super) fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        use tilo_lock::MutexRecover as _;
        self.store.lock_recover()
    }

    /// The store, while its workspace is held alone.
    pub(super) fn store_mut(&self) -> std::sync::MutexGuard<'_, Store> {
        use tilo_lock::MutexRecover as _;
        self.store.lock_recover()
    }

    /// The store as a reader outside the workspace's lock holds it: the same store, read a
    /// command at a time.
    pub(super) fn shared(&self) -> std::sync::Arc<std::sync::Mutex<Store>> {
        std::sync::Arc::clone(&self.store)
    }

    /// Where the store keeps the files of the documents it leaves memory.
    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// Place the reader sets the store writes by `policy` from now on: an agent a resource's
    /// people field names reads it as its person, where the policy lets it read the resource.
    fn follow_policy(&mut self, policy: std::sync::Arc<tilo_action_policy::WorkspacePolicy>) {
        if let Some(readers) = &self.readers {
            readers.follow(policy);
            return;
        }
        let readers = std::sync::Arc::new(PolicyReaders(std::sync::RwLock::new(policy)));
        self.store_mut().set_record_reader(readers.clone());
        self.readers = Some(readers);
    }

    /// The reader set the store knows `doc` by.
    fn readers(&self, doc: &str) -> Option<String> {
        self.store_mut().index().readers(doc).map(str::to_owned)
    }

    /// [`Self::readers`] while the workspace is read, not held alone.
    pub(super) fn known_readers(&self, doc: &str) -> Option<String> {
        use tilo_lock::MutexRecover as _;
        self.store
            .lock_recover()
            .index()
            .readers(doc)
            .map(str::to_owned)
    }

    /// The kinds of the records `write`, staged against the store, writes: what the sealed op
    /// that carries it claims.
    pub(super) fn written_kinds(&self, write: &tilo_resource_store::Written) -> BTreeSet<String> {
        use tilo_lock::MutexRecover as _;
        self.store.lock_recover().written_kinds(write)
    }

    /// Takes a member's `changes` to `doc`, read by `readers`, into the store: each is applied
    /// when `admit` lets its author make it, and kept beside the document without effect when
    /// not. What was applied is kept before this returns. Answers the changes refused, with why.
    ///
    /// # Errors
    /// The document cannot be read, or what was applied cannot be kept.
    pub(super) fn receive(
        &mut self,
        doc: &str,
        readers: &str,
        changes: Vec<Received>,
        admit: &dyn Admit,
    ) -> Result<Vec<(ChangeHash, Refusal)>, String> {
        let before = self.heads(doc);
        let refused = self
            .store_mut()
            .receive(doc, readers, changes, admit)
            .map_err(|error| error.to_string())?;
        self.kept(doc, &before)?;
        Ok(refused)
    }

    /// The heads `doc` stands at, as the index knows it.
    fn heads(&self, doc: &str) -> Vec<ChangeHash> {
        self.store_mut()
            .index()
            .heads(doc)
            .map(<[_]>::to_vec)
            .unwrap_or_default()
    }

    /// Keeps what `doc` took since `before`: its changes appended to its file, then the
    /// index's snapshot of it, so a start reads that instead of the document — and a start
    /// after a stop that never ran (a killed process) reads the same.
    fn kept(&mut self, doc: &str, before: &[ChangeHash]) -> Result<(), String> {
        let root = self.root.clone();
        let docs = root.join(DOCS);
        fs::create_dir_all(&docs).map_err(|error| error.to_string())?;
        let path = docs.join(file_name(doc));
        let snapshot = root.join(INDEX).join(file_name(doc));
        let append = !before.is_empty() && path.exists();
        // The document is saved while the store's lock is held: what is written is what the
        // store has now, not what it had when the lock was taken.
        let bytes = {
            let store = self.store_mut();
            let Some(document) = store.document(doc) else {
                return Ok(());
            };
            if document.get_heads() == before {
                return Ok(());
            }
            let bytes = if append {
                document.save_after(before)
            } else {
                document.save()
            };
            drop(store);
            bytes
        };
        // The snapshot goes before the change it no longer holds: a write killed between
        // them leaves the document whole for the next start to read.
        match fs::remove_file(&snapshot) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        if append {
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .and_then(|mut file| file.write_all(&bytes))
        } else {
            write_whole(&path, &bytes)
        }
        .map_err(|error| error.to_string())?;
        self.used.insert(doc.to_owned(), tilo_clock::now());
        if let Some(changed) = &mut self.batch {
            // A start that finds no snapshot reads the document whole, as it is now.
            *changed = true;
            self.unsnapshotted.insert(doc.to_owned());
            self.evict_idle();
            return Ok(());
        }
        match self.write_snapshot(doc) {
            Ok(()) => {}
            Err(error) => {
                tracing::warn!(%doc, %error, "a resource document's snapshot was not kept");
                self.unsnapshotted.insert(doc.to_owned());
            }
        }
        self.evict_idle();
        self.tell_opened();
        self.publish_live();
        Ok(())
    }

    /// Holds `query` open as `reader` may read it: its first page now, then each page a change
    /// makes different, until the receiver is dropped.
    fn open_query(
        &mut self,
        query: collab_types::store::Query,
        reader: String,
    ) -> tokio::sync::mpsc::UnboundedReceiver<collab_types::store::Page> {
        let (pages, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (watch, page) = {
            let mut store = self.store_mut();
            let watch = store.watch(&query.kind);
            let page = store.query(&query, &may_read(reader.clone()));
            drop(store);
            (watch, page)
        };
        let shown = page.resources.clone();
        if pages.send(page).is_ok() {
            self.opened.insert(
                watch,
                Opened {
                    query,
                    reader,
                    pages,
                    shown,
                },
            );
        } else {
            self.store_mut().unwatch(watch);
        }
        receiver
    }

    /// Sends each open query whose kind a change touched its page again, when what it shows
    /// changed, and lets go of every query whose app no longer holds it.
    fn tell_opened(&mut self) {
        use tilo_lock::MutexRecover as _;
        if self.opened.is_empty() {
            return;
        }
        let changed = self.store_mut().changed();
        let closed: Vec<u64> = self
            .opened
            .iter()
            .filter(|(_, open)| open.pages.is_closed())
            .map(|(watch, _)| *watch)
            .collect();
        if !closed.is_empty() {
            let mut store = self.store_mut();
            for watch in closed {
                store.unwatch(watch);
            }
        }
        self.opened.retain(|_, open| !open.pages.is_closed());
        let due: Vec<u64> = self
            .opened
            .keys()
            .filter(|watch| changed.contains(watch))
            .copied()
            .collect();
        for watch in due {
            let Some(open) = self.opened.get(&watch) else {
                continue;
            };
            let page = {
                let store = self.store.lock_recover();
                store.query(&open.query, &may_read(open.reader.clone()))
            };
            let Some(open) = self.opened.get_mut(&watch) else {
                continue;
            };
            if page.resources == open.shown {
                continue;
            }
            open.shown.clone_from(&page.resources);
            if open.pages.send(page).is_err() {
                self.store_mut().unwatch(watch);
                self.opened.remove(&watch);
            }
        }
    }

    /// Lets every document nobody wrote for [`IDLE`] go from memory, kept whole with its
    /// snapshot.
    fn evict_idle(&mut self) {
        let now = tilo_clock::now();
        let idle: Vec<String> = self
            .used
            .iter()
            .filter(|(_, used)| now.saturating_duration_since(**used) >= IDLE)
            .map(|(doc, _)| doc.clone())
            .collect();
        for doc in idle {
            self.let_go(&doc);
        }
    }

    /// Keeps `doc` whole with its snapshot, and lets it go from memory.
    fn let_go(&mut self, doc: &str) {
        self.used.remove(doc);
        let Some(bytes) = self.store_mut().evict(doc) else {
            return;
        };
        if let Err(error) = self.keep_whole(doc, &bytes) {
            tracing::warn!(%doc, %error, "a resource document was not kept whole");
        }
    }

    /// Writes `doc` whole as `bytes`, then the index's snapshot of it.
    fn keep_whole(&mut self, doc: &str, bytes: &[u8]) -> std::io::Result<()> {
        let docs = self.root.join(DOCS);
        fs::create_dir_all(&docs)?;
        write_whole(&docs.join(file_name(doc)), bytes)?;
        self.write_snapshot(doc)
    }

    /// Writes the index's snapshot of `doc` beside it, at the heads it stands at: what a
    /// start reads instead of the document.
    fn write_snapshot(&mut self, doc: &str) -> std::io::Result<()> {
        let index = self.root.join(INDEX);
        fs::create_dir_all(&index)?;
        let Some(snapshot) = self.store_mut().snapshot(doc) else {
            return Ok(());
        };
        let heads: Vec<[u8; 32]> = self.heads(doc).iter().map(|head| head.0).collect();
        let kept = postcard::to_stdvec(&(heads, snapshot))
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        write_whole(&index.join(file_name(doc)), &kept)?;
        self.unsnapshotted.remove(doc);
        Ok(())
    }

    /// Keeps every document in memory whole with its snapshot, as the service stops, so the
    /// next start reads snapshots rather than documents.
    pub(super) fn keep(&mut self) {
        let docs: Vec<String> = self.used.keys().cloned().collect();
        for doc in docs {
            self.let_go(&doc);
        }
    }
}

impl Drop for Resources {
    fn drop(&mut self) {
        if !self.unsnapshotted.is_empty() {
            self.keep();
        }
    }
}

/// A document's snapshot: the heads it was taken at, and the index's bytes of it.
type Snapshot = (Vec<ChangeHash>, Vec<u8>);

fn read_snapshot(root: &Path, doc: &str) -> Option<Snapshot> {
    let bytes = fs::read(root.join(INDEX).join(file_name(doc))).ok()?;
    let (heads, snapshot): (Vec<[u8; 32]>, Vec<u8>) = postcard::from_bytes(&bytes).ok()?;
    Some((heads.into_iter().map(ChangeHash).collect(), snapshot))
}

/// The snapshot kept of each of `docs`, in their order, read a few at a time: a start reads a
/// file per document of a workspace's chain of parts, and one after another made it wait for
/// each in turn.
fn read_snapshots(root: &Path, docs: &[(String, PathBuf)]) -> Vec<Option<Snapshot>> {
    let serial = || {
        docs.iter()
            .map(|(doc, _)| read_snapshot(root, doc))
            .collect::<Vec<_>>()
    };
    #[cfg(target_arch = "wasm32")]
    {
        serial()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        /// Snapshots read at once.
        const AT_ONCE: usize = 4;
        let workers = std::thread::available_parallelism()
            .map_or(1, std::num::NonZeroUsize::get)
            .clamp(1, AT_ONCE)
            .min(docs.len().max(1));
        if workers == 1 {
            return serial();
        }
        let next = std::sync::atomic::AtomicUsize::new(0);
        let snapshots: std::sync::Mutex<Vec<Option<Snapshot>>> =
            std::sync::Mutex::new(vec![None; docs.len()]);
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let Some((doc, _)) = docs.get(at) else {
                            break;
                        };
                        let read = read_snapshot(root, doc);
                        let mut snapshots = snapshots
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if let Some(held) = snapshots.get_mut(at) {
                            *held = read;
                        }
                    }
                });
            }
        });
        snapshots
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The changes a member's write of a store document carries, and the reader set the document
/// names when the write founds it.
fn changes_of(
    mutation: collab_types::DocumentMutation,
) -> Result<(Vec<AmChange>, Option<String>), String> {
    match mutation {
        collab_types::DocumentMutation::Snapshot(bytes) => {
            let document = Automerge::load(&bytes).map_err(|error| error.to_string())?;
            let readers = match document.get(ROOT, "w") {
                Ok(Some((value, _))) => value.as_str().map(str::to_owned),
                _ => None,
            };
            Ok((document.get_changes(&[]), readers))
        }
        collab_types::DocumentMutation::Incremental(bytes) => {
            let change = AmChange::from_bytes(bytes).map_err(|error| error.to_string())?;
            Ok((vec![change], None))
        }
    }
}

impl HostedWorkspace {
    /// The workspace's store, with the types and verbs of every app surface the device knows
    /// (`surfaces`) declared: what this device's writes and the members' are judged by.
    fn store_with_surfaces<'a>(
        &'a mut self,
        surfaces: &collab_types::app_surface::Surfaces,
    ) -> std::sync::MutexGuard<'a, Store> {
        let named = surfaces.registered();
        // The system's own kind, which no app declares: a reference to another workspace's
        // record, whichever app's it is.
        {
            let mut store = self.resources.store_mut();
            if store
                .schema()
                .get(tilo_resource_store::reference::KIND)
                .is_none()
            {
                let _ = store.declare(tilo_resource_store::reference::schema());
            }
        }
        if self.resources.declared != named {
            self.resources.declared = named;
            {
                let mut store = self.resources.store_mut();
                for (app, surface) in surfaces.resources() {
                    if let Err(error) = store.declare(Schema::of_app(&app, &surface)) {
                        tracing::error!(%error, "the store keeps the type its surface declared before");
                    }
                }
            }
            // A type that says when it is live lists what the store held before it said so.
            self.resources.publish_live();
        }
        self.resources.store_mut()
    }

    /// Takes `author`'s write `envelope` of the store document `doc` of `app` into the
    /// workspace's store, `named` the reader set the write names in the clear. A sealed write is
    /// read with its reader set's keys, under the scope the document's id derives; a device that
    /// holds none keeps and forwards it, as it does any sealed write, and indexes nothing of it.
    ///
    /// A write that is one of an action's writes to several documents is taken with the others,
    /// once every one is here, unless `whole` says the caller keeps all of them now: a store
    /// that stops between two of them, or never gets one, holds none of them.
    ///
    /// # Errors
    /// The write does not open or decode, or the store cannot keep it.
    ///
    /// Whether any change of it took effect, or it waits for the rest of its action.
    pub(super) async fn take_resources(
        &mut self,
        author: [u8; 32],
        (app, doc): (&str, &str),
        envelope: &collab_types::SignedOp,
        named: Option<&str>,
        whole: bool,
    ) -> Result<bool, String> {
        // The reader set picks the key: every member's for `*`, else the set the store met the
        // document under, else the set the write names in the clear, when the id names that
        // set's digest.
        let in_id = collab_types::store_document_readers(doc).ok_or("not a resource document")?;
        let known = if in_id == collab_types::EVERY_MEMBER {
            Some(collab_types::EVERY_MEMBER.to_owned())
        } else {
            self.resources.readers(doc).or_else(|| {
                named
                    .filter(|readers| {
                        collab_types::reader_set_name(readers) == in_id
                            && collab_types::Readers::parse(readers).is_some()
                    })
                    .map(str::to_owned)
            })
        };
        let payload = match envelope.encryption {
            None => envelope.payload.clone(),
            Some(metadata) => {
                let Some(readers) = &known else {
                    return Ok(false);
                };
                let keys = match metadata.scheme {
                    collab_types::EncryptionScheme::DocumentContentKey { group, .. } => {
                        let Some(device) = &self.device else {
                            return Ok(false);
                        };
                        let workspace = collab_types::WorkspaceId::parse(self.workspace())
                            .ok_or("invalid resource workspace")?;
                        let named =
                            (readers != collab_types::EVERY_MEMBER).then_some(readers.as_str());
                        let scope = neighbour_wire::KeyScope::document(workspace, app, doc, named)
                            .filter(|scope| scope.group() == group)
                            .ok_or("the encryption group belongs to another document")?;
                        device.group_keys(&scope.to_string(), readers, false).await
                    }
                    _ => self.reader_keys(readers).await,
                };
                let Some(Ok(keys)) = keys else {
                    return Ok(false);
                };
                let ciphertext =
                    collab_types::sealed_ciphertext(envelope).map_err(|error| error.to_string())?;
                // Sealed for the document's own scope, which its id derives, and no other.
                let scope = collab_types::document_artifact_id(app, self.workspace(), doc);
                keys.open_content(scope, &metadata, &ciphertext)
                    .map_err(|error| error.to_string())?
                    .to_vec()
            }
        };
        let (payload, action) =
            collab_types::WorkspaceCommit::split(&payload).map_err(|error| error.to_string())?;
        if action
            .as_ref()
            .is_some_and(|action| envelope.author.as_bytes() != &action.author.device)
        {
            return Err("a write carries an action another device made".into());
        }
        // A change larger than one frame comes as segments, each a write of its own: the store
        // takes it once the last is here.
        let Some(mutation) = self.resources.transfers.take(author, payload)? else {
            return Ok(false);
        };
        let (changes, named_in_document) = changes_of(mutation)?;
        let readers = known
            .or(named_in_document)
            .ok_or("a write of a resource document of readers this device never met")?;
        let Some(action) = action.filter(|action| !whole && action.parts.len() > 1) else {
            return self.receive_changes(author, (app, doc), envelope, &readers, changes);
        };
        let id = action.id();
        self.resources
            .actions
            .entry(id)
            .or_default()
            .push(ActionWrite {
                author,
                app: app.to_owned(),
                doc: doc.to_owned(),
                envelope: envelope.clone(),
                readers,
                changes,
            });
        if !self.action_here(&action, &id) {
            return Ok(true);
        }
        let mut applied = false;
        for write in self.resources.actions.remove(&id).unwrap_or_default() {
            applied |= self.receive_changes(
                write.author,
                (&write.app, &write.doc),
                &write.envelope,
                &write.readers,
                write.changes,
            )?;
        }
        Ok(applied)
    }

    /// The documents of the writes held for actions whose other writes are not here yet.
    pub(super) fn held_action_documents(&self) -> Vec<String> {
        self.resources
            .actions
            .values()
            .flatten()
            .map(|write| write.doc.clone())
            .collect()
    }

    /// Whether every write of `action`, `id`, is here: held for it, or in the store already.
    fn action_here(&self, action: &collab_types::WorkspaceCommit, id: &[u8; 32]) -> bool {
        use tilo_lock::MutexRecover as _;
        let held = self.resources.actions.get(id);
        let mut store = self.resources.store.lock_recover();
        action.parts.iter().all(|part| {
            part.heads.iter().all(|head| {
                let hash = ChangeHash(*head);
                held.is_some_and(|writes| {
                    writes.iter().any(|write| {
                        write.doc == part.document
                            && write.changes.iter().any(|change| change.hash() == hash)
                    })
                }) || store.has_change(&part.document, &hash)
            })
        })
    }

    /// Take `changes`, `author`'s write `envelope` of `doc` of `app` for `readers`, into the
    /// store: whether any of them took effect.
    fn receive_changes(
        &mut self,
        author: [u8; 32],
        (app, doc): (&str, &str),
        envelope: &collab_types::SignedOp,
        readers: &str,
        changes: Vec<AmChange>,
    ) -> Result<bool, String> {
        // Every change of one write is its author's: whether they may write the document is
        // asked once, of the workspace's one permission check, and so is who they are.
        let now = tilo_clock::wall();
        let permitted = self.may_start(author, (app, doc), now).is_ok();
        let permissions = self.peer().permissions(author, now);
        let author_person = match permissions {
            Ok(member) => member
                .acting
                .by
                .member_id()
                .and_then(|member| member.person_id())
                .and_then(person_bytes),
            Err(error) => {
                tracing::warn!(%error, "a write's author is no member of the workspace");
                None
            }
        };
        // The origin's write is the platform's own, and no app action makes it: it is admitted
        // only for the account whose sign-in its writer holds, in that account's place, read by
        // the person who wrote it. What it needs is read once for the whole write. Every other
        // change is its author's to make, and a platform verb's wider write the platform's.
        let stamps: Vec<collab_types::Stamp> = origin_stamps(&changes)
            .into_iter()
            .map(|(_, stamp)| stamp)
            .collect();
        let origin = if stamps.is_empty() {
            None
        } else {
            let ground = self.origin_ground(changes.iter().map(|change| (doc, change)));
            self.origin_leave(author, &stamps, false, &ground)
        };
        // Every other change is its author's to make: the grant the governance it was written
        // under gives them at its verb and each kind it touches, read once for the batch.
        let grants = self.verb_grants(app, doc, envelope, author, now);
        let admit = move |proposal: &tilo_resource_store::Proposal<'_>| {
            if proposal
                .stamp
                .is_some_and(|stamp| stamp.verb == collab_types::Stamp::ORIGIN_VERB)
            {
                origin.as_ref().is_some_and(|leave| leave.admits(proposal))
            } else {
                permitted
                    && platform_write_belongs(proposal.stamp, author_person)
                    && proposal.stamp.as_ref().is_none_or(|stamp| {
                        proposal.touched.resources.iter().all(|(_, touch)| {
                            touch.kind.as_deref().is_none_or(|kind| {
                                grants.may(&author, &stamp.verb, kind)
                                    && grants.may_take_back(&author, stamp, kind)
                            })
                        })
                    })
            }
        };
        // A sealed write was decided, by members without its key, by the kinds its claims name;
        // the store refuses each change unless they are the kinds it reads. A claim that names no
        // kind was decided against the record's own surface, and is left to that.
        let claimed = |change: &AmChange| {
            envelope.encryption.as_ref()?;
            envelope
                .claims
                .iter()
                .find(|claim| claim.change == change.hash().0)
                .and_then(|claim| collab_types::claim::whole_edit_kinds(&claim.action))
                .filter(|kinds| !kinds.is_empty())
        };
        let received: Vec<Received> = changes
            .into_iter()
            .map(|change| Received {
                claimed: claimed(&change),
                change,
                author,
            })
            .collect();
        let taken = received.len();
        let refused = self.resources.receive(doc, readers, received, &admit)?;
        let applied = refused.len() < taken;
        for (change, refusal) in refused {
            tracing::info!(workspace = %self.workspace(), %doc, %change, ?refusal, "a member's change to a resource was refused");
        }
        Ok(applied)
    }

    /// What an origin's write is judged by, read once for `stamps`: the devices that may author
    /// it, each with the access it wrote under, where the records it touches and the records above
    /// them stand (`places`, read once for the batch), and the outside kind each kind declares.
    /// `None` when the write carries no change of the origin's.
    fn origin_leave(
        &self,
        author: [u8; 32],
        stamps: &[collab_types::Stamp],
        carried: bool,
        ground: &OriginGround,
    ) -> Option<OriginLeave> {
        if stamps.is_empty() {
            return None;
        }
        let now = tilo_clock::wall();
        // The device a change names is the one the workspace service wrote it for, and the
        // device that signed it is the one it came from: either may be the one that holds the
        // account's sign-in, and both are read against the membership now.
        let mut devices = BTreeSet::from([author]);
        devices.extend(stamps.iter().filter_map(device_of));
        let mut writers = std::collections::HashMap::new();
        for device in devices {
            let access = self.peer().permissions(device, now);
            if let Ok(access) = access {
                writers.insert(device, access);
            }
        }
        Some(OriginLeave {
            writers,
            places: std::sync::Arc::clone(&ground.places),
            origins: std::sync::Arc::clone(&ground.origins),
            carried,
        })
    }

    /// What every origin's leave of `changes` reads, read once for the batch.
    fn origin_ground<'a>(
        &self,
        changes: impl IntoIterator<Item = (&'a str, &'a AmChange)>,
    ) -> OriginGround {
        use tilo_lock::MutexRecover as _;
        let store = self.resources.store.lock_recover();
        OriginGround {
            places: std::sync::Arc::new(origin_places(&store, changes)),
            origins: std::sync::Arc::new(declared_origins(&store)),
        }
    }

    /// The origin's leave for each document the `writes` touch by the origin's write, read once
    /// for the batch these writes are checked in.
    fn origin_leaves(
        &self,
        author: [u8; 32],
        writes: &[tilo_resource_store::Written],
        carried: bool,
    ) -> std::collections::HashMap<String, OriginLeave> {
        let mut stamps: BTreeMap<String, Vec<collab_types::Stamp>> = BTreeMap::new();
        for write in writes {
            if let Some(stamp) = collab_types::Stamp::of(&write.change)
                && stamp.verb == collab_types::Stamp::ORIGIN_VERB
            {
                stamps.entry(write.doc.clone()).or_default().push(stamp);
            }
        }
        if stamps.is_empty() {
            return std::collections::HashMap::new();
        }
        let ground = self.origin_ground(
            writes
                .iter()
                .map(|write| (write.doc.as_str(), &write.change)),
        );
        stamps
            .into_iter()
            .filter_map(|(doc, stamps)| {
                self.origin_leave(author, &stamps, carried, &ground)
                    .map(|leave| (doc, leave))
            })
            .collect()
    }

    /// What a call of `app`'s action `action` needs, as the app's declaration of it in this
    /// workspace's store says ([`VerbGrants::call_need`]), with every surface this device knows
    /// (`surfaces`) declared: the kind it is judged at and the field it sits under. `None` for an
    /// action no declaration names.
    pub(crate) fn call_need(
        &mut self,
        surfaces: &collab_types::app_surface::Surfaces,
        app: &str,
        action: &str,
    ) -> Option<tilo_actions::ActionNeed> {
        VerbGrants::call_need(self.store_with_surfaces(surfaces).schema(), app, action)
    }

    /// The grant lookup for a write of `app` by `author`: what the governance the write names
    /// gives them, read once for the batch.
    fn verb_grants(
        &self,
        app: &str,
        doc: &str,
        envelope: &collab_types::SignedOp,
        author: [u8; 32],
        now: web_time::SystemTime,
    ) -> VerbGrants {
        let access = self
            .peer()
            .permissions_as_written(app, doc, envelope, now)
            .ok();
        VerbGrants::of(access, app, author, now)
    }

    async fn draft_resource_document(
        &self,
        peer: &tilo_peer_state::StatePeer,
        device: &super::MemberDevice,
        app: &str,
        write: &tilo_resource_store::Written,
        action: Option<&collab_types::WorkspaceCommit>,
    ) -> Result<Vec<collab_types::SignedOp>, String> {
        let now = tilo_clock::wall();
        let named = (collab_types::is_store_document(&write.doc)
            && write.readers != collab_types::EVERY_MEMBER)
            .then_some(write.readers.as_str());
        let isolated = peer.requires_document_keys(app, &write.doc, now)?;
        let group = if isolated {
            let workspace =
                collab_types::WorkspaceId::parse(self.workspace()).ok_or("invalid workspace id")?;
            neighbour_wire::KeyScope::document(workspace, app, &write.doc, named)
                .ok_or_else(|| {
                    format!(
                        "{app}'s document {} names a reader set other than its readers {}",
                        write.doc, write.readers
                    )
                })?
                .to_string()
        } else {
            self.workspace().to_owned()
        };
        let key_device = self
            .device
            .as_ref()
            .ok_or("this device holds no group keys")?;
        let keys = match key_device
            .group_keys(&group, &write.readers, isolated)
            .await
        {
            Some(Ok(keys)) => keys,
            // A record written under a reader set this device takes part in and holds no keys
            // of yet, as one it came to when the workspace gained a member: open the group as
            // its reads of it do, and write under the keys that brings.
            Some(Err(_)) if !isolated => {
                match key_device.group_keys(&group, &write.readers, true).await {
                    Some(Ok(keys)) => keys,
                    Some(Err(error)) => return Err(error),
                    None => {
                        return Err(
                            "this device holds no keys of the document's readers yet".into()
                        );
                    }
                }
            }
            Some(Err(error)) => return Err(error),
            None => return Err("this device holds no keys of the document's readers yet".into()),
        };
        let founding = !peer.holds_document(app, &write.doc);
        let bytes = if founding {
            if !write.change.deps().is_empty() {
                return Err("the workspace has not opened this resource's signed history".into());
            }
            let mut document = Automerge::new();
            document
                .apply_changes([write.change.clone()])
                .map_err(|error| error.to_string())?;
            document.save()
        } else {
            write.change.raw_bytes().to_vec()
        };
        let (envelopes, _) = tilo_peer_state::owner::draft_local_write(
            peer,
            self.workspace(),
            &device.device(),
            tilo_peer_state::owner::LocalWrite {
                app,
                document: &write.doc,
                readers: named,
                changes: &bytes,
                founding,
                snapshot: founding,
                sealed: Some(&keys),
                commit: action,
                now: tilo_clock::wall(),
            },
        )
        .map_err(|error| error.to_string())?;
        // A change larger than one frame is drafted as segments, each a write of its own, and
        // each names the kinds the change writes.
        let kinds: collab_types::claim::RecordKinds =
            std::iter::once((write.change.hash().0, self.resources.written_kinds(write)))
                .filter(|(_, kinds)| !kinds.is_empty())
                .collect();
        envelopes
            .into_iter()
            .map(|mut envelope| {
                // The write names the version of the app the workspace runs, as a change of its
                // record names it: a change that names none — as a record this device writes on
                // its own would — is held in a workspace that runs a version, and waits for a
                // version nobody ever names.
                envelope.surface = peer.running_surface(app);
                if envelope.encryption.is_some() {
                    peer.name_record_kinds((app, &write.doc), &mut envelope, &kinds);
                }
                collab_types::sign_op(&mut envelope, &*device.signer)
                    .map_err(|error| error.to_string())?;
                Ok(envelope)
            })
            .collect()
    }
}

/// The device's own reads and writes of a workspace's store, for the tests of the work
/// its agents do on an app's resources.
impl Workspaces {
    /// Answer the read `command` from the store of `workspace`, with only what `person` (their
    /// root, in hex) may read: what a test of the agents' work on an app's resources stands on.
    ///
    /// # Errors
    /// The workspace is not held here, or `command` writes.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn read_store_as(
        &self,
        workspace: &str,
        person: &str,
        command: &collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, String> {
        if command.writes() {
            return Err("a read of the store writes nothing".to_owned());
        }
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut hosted = state.lock().await;
        let store = hosted.store_with_surfaces(&self.inner.host.surfaces);
        let answer = tilo_resource_store::wire::read(&store, command, &may_read(person.to_owned()))
            .unwrap_or(collab_types::store::wire::Reply::Written);
        drop(store);
        drop(hosted);
        Ok(answer)
    }

    /// Take `documents` (each whole, by its id) into the store of `workspace`, as a member's
    /// device's writes reach it by sync: what the tests of the agents' work on an app's
    /// resources stand on, the app running where it runs, on a device.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn take_documents(&self, workspace: &str, documents: Vec<(String, Vec<u8>)>) {
        let state = super::fixtures::needed(
            self.workspace(workspace).await,
            &format!("a test's documents reach the workspace {workspace}"),
        );
        let mut hosted = state.lock().await;
        let mut store = hosted.store_with_surfaces(&self.inner.host.surfaces);
        for (doc, bytes) in documents {
            super::fixtures::needed(
                store.load(&doc, &bytes),
                &format!("a test's document {doc} loads"),
            );
        }
        drop(store);
        drop(hosted);
    }
}

/// What a test reads of a workspace's store and the replica beside it: the documents the store
/// placed, what its index holds of them, and the writes the replica keeps of them.
///
/// A test outside this package reads a member's write back through these, as it reads any other
/// answer of the service: the store's own lock and projection stay here.
#[cfg(feature = "test-support")]
impl Workspaces {
    /// How many of `workspace`'s documents the store read into memory: what a start that read
    /// the index's snapshots leaves at none.
    pub async fn store_in_memory(&self, workspace: &str) -> usize {
        let Ok(state) = self.workspace(workspace).await else {
            return 0;
        };
        let mut hosted = state.lock().await;
        hosted
            .store_with_surfaces(&self.inner.host.surfaces)
            .in_memory()
    }

    /// How many queries an app holds open on `workspace`'s store.
    pub async fn store_queries_opened(&self, workspace: &str) -> usize {
        let Ok(state) = self.workspace(workspace).await else {
            return 0;
        };
        let hosted = state.lock().await;
        hosted.resources.opened.len()
    }

    /// Hold `query` open for the app `context` names, in its workspace, as
    /// [`Self::open_resources`] does: what a test of the pages a write sends reads.
    ///
    /// # Errors
    /// As [`Self::open_resources`].
    pub async fn open_resource_query(
        &self,
        context: &tilo_broker_core::services::ResourceRequestContext,
        query: collab_types::store::Query,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<collab_types::store::Page>, String> {
        self.open_resources(context, query).await
    }

    /// Whether `workspace`'s replica holds document `doc` of `app`: what the store placed and
    /// what the member's device wrote there.
    pub async fn peer_holds(&self, workspace: &str, app: &str, doc: &str) -> bool {
        let Ok(state) = self.workspace(workspace).await else {
            return false;
        };
        let hosted = state.lock().await;
        hosted.peer().holds_document(app, doc)
    }

    /// The kinds the replica's sealed writes of `doc`, `app`'s, claim, each write's as one set:
    /// what a member without the document's key decides the edit after the first write by.
    pub async fn sealed_claims(
        &self,
        workspace: &str,
        app: &str,
        doc: &str,
    ) -> Vec<std::collections::BTreeSet<String>> {
        let Ok(state) = self.workspace(workspace).await else {
            return Vec::new();
        };
        let hosted = state.lock().await;
        hosted
            .peer()
            .publication()
            .sealed_history(app, doc)
            .iter()
            .flat_map(|op| op.claims.iter())
            .filter_map(|claim| collab_types::claim::whole_edit_kinds(&claim.action))
            .collect()
    }

    /// Take the member's write `envelope` of `doc`, `app`'s, into `workspace`'s store, `author`
    /// the device that wrote it: whether any change of it took effect, as
    /// [`Self::absorb_member_write`] takes a whole body.
    ///
    /// # Errors
    /// The write does not open or decode, or the store cannot keep it.
    pub async fn take_member_write(
        &self,
        workspace: &str,
        (app, doc): (&str, &str),
        author: [u8; 32],
        envelope: &collab_types::SignedOp,
    ) -> Result<bool, String> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut hosted = state.lock().await;
        drop(hosted.store_with_surfaces(&self.inner.host.surfaces));
        hosted
            .take_resources(author, (app, doc), envelope, None, false)
            .await
    }

    /// Hold `workspace` as a write holds it from the moment it is drafted until its change is
    /// kept, until the answer is dropped: what a test that reads a page while a write holds the
    /// workspace holds.
    ///
    /// # Errors
    /// This device holds no such workspace.
    pub async fn hold_workspace(&self, workspace: &str) -> Result<impl Send, String> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        Ok(state.lock_owned().await)
    }

    /// Keep `commit`, a signed body this device's own roles wrote, and carry it, answering from
    /// where the frames of `workspace_id` stand.
    ///
    /// # Errors
    /// The body does not decode, or the document could not be kept.
    pub fn carry_document(
        &self,
        workspace_id: tilo_server_protocol::WorkspaceId,
        author: [u8; 32],
        commit: &crate::workspace_documents::HostCommit,
    ) -> Result<u64, String> {
        self.inner.records.carry(workspace_id, author, commit)
    }
}

impl Workspaces {
    /// Restore the store projection of a sealed frame that passed peer admission.
    pub(super) async fn replay_resource_write(
        &self,
        hosted: &mut HostedWorkspace,
        author: [u8; 32],
        message: &neighbour_wire::NeighbourMsg,
    ) -> bool {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            readers,
            ..
        } = message
        else {
            return false;
        };
        if !is_store_document(document_id)
            || envelope.encryption.is_none()
            || !hosted
                .peer()
                .publication()
                .has_sealed_write(app_id, document_id, envelope)
        {
            return false;
        }
        drop(self.store_of(&mut *hosted));
        if let Err(error) = hosted
            .take_resources(
                author,
                (app_id, document_id),
                envelope,
                readers.as_deref(),
                false,
            )
            .await
        {
            tracing::warn!(workspace = %hosted.workspace(), %document_id, %error, "a retained resource write was not restored");
        }
        true
    }

    /// Takes `message`, `author`'s write, into `workspace`'s store when it writes a document the
    /// store placed, and hands it back so the workspace's replica keeps it; any other write is
    /// handed back untaken.
    pub(super) async fn absorb_resources(
        &self,
        workspace: &str,
        author: [u8; 32],
        message: neighbour_wire::NeighbourMsg,
    ) -> Result<Option<neighbour_wire::NeighbourMsg>, String> {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            document_id,
            envelope,
            readers,
            ..
        } = &message
        else {
            return Ok(Some(message));
        };
        if !is_store_document(document_id) {
            return Ok(Some(message));
        }
        if envelope
            .encryption
            .and_then(|metadata| metadata.retention())
            .is_none()
        {
            return Ok(None);
        }
        let hosted = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut hosted = hosted.lock().await;
        let now = tilo_clock::wall();
        if hosted
            .may_start(author, (app_id, document_id), now)
            .is_err()
        {
            return Ok(None);
        }
        let mut candidate = hosted.peer().clone();
        candidate
            .receive_from(
                tilo_peer_state::TransportOrigin::RetainedServerDelta(author),
                message.clone(),
                now,
            )
            .map_err(|error| error.to_string())?;
        if !candidate
            .publication()
            .has_sealed_write(app_id, document_id, envelope)
        {
            return Ok(None);
        }
        // The store judges the change by the types and verbs of the app surfaces this device
        // knows, so they are declared before the change reaches it, as a restored write finds
        // them ([`Self::replay_resource_write`]): a store that declared none admits none.
        drop(self.store_of(&mut hosted));
        if hosted
            .take_resources(
                author,
                (app_id, document_id),
                envelope,
                readers.as_deref(),
                false,
            )
            .await?
        {
            // Whoever follows the workspace's writes (its bots) is woken by a resource's
            // change as by any, while the workspace is held, so it reads the change.
            self.tell_followers(&hosted, author, &message, false).await;
            self.wrote_frame(hosted.workspace(), &message);
        }
        drop(hosted);
        // The workspace's own replica keeps the document the store indexed, as it keeps every
        // document a member wrote: a change of it is drafted after it, and the workspace's other
        // members follow it as they follow any.
        Ok(Some(message))
    }

    /// Commit an existing, staged resource update while the workspace remains held.
    /// Check the same store and peer admissions that will take the retained frame.
    async fn commit_resource_update(
        &self,
        hosted: &mut HostedWorkspace,
        device: &super::MemberDevice,
        app: &str,
        write: &tilo_resource_store::Written,
        still_live: impl FnOnce(&HostedWorkspace) -> bool,
    ) -> Result<(), String> {
        let document = &write.doc;
        let author = device.device();
        let now = tilo_clock::wall();
        hosted.may_start(author, (app, document), now)?;
        // A staged edit is a delta. Recover its signed base from the same
        // retained history the workspace replays on open.
        if !hosted.peer().holds_document(app, document) {
            self.recall(hosted).await?;
            if !hosted.peer().holds_document(app, document) {
                return Err("the workspace has not opened this resource's signed history".into());
            }
        }
        let mut candidate = hosted.peer().clone();
        let envelope = self
            .admit_resource_write(hosted, &mut candidate, device, app, (write, None))
            .await?;
        if !still_live(hosted) {
            return Err("the resource writer is no longer authorized".into());
        }
        let messages = Self::resource_write_messages(
            hosted.workspace_id(),
            device.device(),
            app,
            write,
            &envelope,
        )?;
        self.keep_resource_write(hosted, device, app, (write, true), &messages, true)
            .await
    }

    async fn admit_resource_write(
        &self,
        hosted: &HostedWorkspace,
        candidate: &mut tilo_peer_state::StatePeer,
        device: &super::MemberDevice,
        app: &str,
        (write, action): (
            &tilo_resource_store::Written,
            Option<&collab_types::WorkspaceCommit>,
        ),
    ) -> Result<Vec<collab_types::SignedOp>, String> {
        use neighbour_wire::NeighbourMsg;

        let document = &write.doc;
        let author = device.device();
        let now = tilo_clock::wall();
        hosted.may_start(author, (app, document), now)?;
        let envelopes = hosted
            .draft_resource_document(candidate, device, app, write, action)
            .await?;
        let named = (collab_types::is_store_document(document)
            && write.readers != collab_types::EVERY_MEMBER)
            .then_some(write.readers.as_str());
        let device_keys = hosted
            .device
            .as_ref()
            .ok_or("this device holds no group keys")?;
        let scoped = hosted.peer().requires_document_keys(app, document, now)?;
        let keys = super::sealed_documents::read_document_keys_for(
            device_keys,
            hosted.workspace(),
            (app, document),
            scoped,
            scoped,
            Some(write.readers.clone()),
        )
        .await?;
        let mut taken = None;
        for envelope in &envelopes {
            let message = NeighbourMsg::AppDocument {
                peer: neighbour_wire::peer_handle(&author),
                app_id: app.to_owned(),
                cluster_id: hosted.workspace().to_owned(),
                document_id: document.clone(),
                envelope: Box::new(envelope.clone()),
                readers: named.map(str::to_owned),
            };
            let report = candidate
                .receive_from_report(
                    tilo_peer_state::TransportOrigin::RetainedServerDelta(author),
                    message,
                    now,
                )
                .map_err(|error| error.to_string())?;
            taken = Some((
                report.summary().clone(),
                format!("{:?}", report.primary()),
                matches!(
                    report.primary(),
                    tilo_peer_state::InputDisposition::Accepted(_)
                ),
            ));
        }
        let (taken, decided, accepted) =
            taken.ok_or("a resource change was drafted as no write")?;
        // One write of an action across several documents reads only once the action is whole:
        // the workspace took it in when nothing held it back or refused it.
        if action.is_some_and(|action| action.parts.len() > 1)
            && !taken.deferred
            && taken.incomplete.is_none()
            && taken.rejected == 0
        {
            return Ok(envelopes);
        }
        let opened = candidate
            .publication()
            .opened_document(app, document, &mut keys.open())?
            .ok_or_else(|| {
                // A write the workspace keeps nothing of is held back or refused: saying only
                // that its document does not open leaves the reason nowhere.
                format!(
                    "the workspace took no write of {document} of {app}: {decided}, held back {} \
                     (waiting for {}), duplicate {}, refused {}",
                    taken.deferred,
                    taken.incomplete.as_deref().unwrap_or("nothing"),
                    taken.duplicate,
                    taken.rejected
                )
            })?;
        // A sealed document's writes are no changes of the cleartext this device opens it to:
        // what says one landed is the disposition the admission reported for it. A document
        // that travels in the clear is looked for in the document itself.
        let sealed = candidate
            .publication()
            .held_document(app, document)
            .is_some_and(|held| held.is_sealed());
        resource_write_landed(&opened, sealed, accepted, write, &decided, &taken)?;
        Ok(envelopes)
    }

    /// The frames `write`'s `envelopes` go out as, `author`'s for `app`: what
    /// [`crate::workspace_documents::WorkspaceDocuments::carry_all`] keeps once the store holds
    /// the write.
    fn resource_write_messages(
        workspace_id: tilo_server_protocol::WorkspaceId,
        author: [u8; 32],
        app: &str,
        write: &tilo_resource_store::Written,
        envelopes: &[collab_types::SignedOp],
    ) -> Result<Vec<neighbour_wire::NeighbourMsg>, String> {
        let document = &write.doc;
        let named = (collab_types::is_store_document(document)
            && write.readers != collab_types::EVERY_MEMBER)
            .then_some(write.readers.as_str());
        envelopes
            .iter()
            .map(|envelope| {
                Self::local_document_message(workspace_id, author, app, document, named, envelope)
            })
            .collect()
    }

    /// Have the store take `write`, and carry its frames `messages` to every device that follows
    /// the workspace once it holds it: a frame is carried only after the write is kept, so a
    /// write the store or the workspace refuses leaves nothing of it on the wire, under the
    /// numbers its store's next draft takes again.
    async fn keep_resource_write(
        &self,
        hosted: &mut HostedWorkspace,
        device: &super::MemberDevice,
        app: &str,
        (write, whole): (&tilo_resource_store::Written, bool),
        messages: &[neighbour_wire::NeighbourMsg],
        compact: bool,
    ) -> Result<(), String> {
        let author = device.device();
        let document = &write.doc;
        // A write the store takes later, with the rest of its action, was checked with them.
        if whole {
            self.store_of(&mut *hosted)
                .check_write(write, author)
                .map_err(|refusal| format!("the resource change was refused: {refusal:?}"))?;
        }
        let count = messages.len();
        for (index, message) in messages.iter().enumerate() {
            // The store takes the change with its last segment.
            let last = index.saturating_add(1) == count;
            let neighbour_wire::NeighbourMsg::AppDocument {
                envelope, readers, ..
            } = message
            else {
                return Err("a resource write must name its document".into());
            };
            if hosted
                .take_resources(author, (app, document), envelope, readers.as_deref(), whole)
                .await?
                != last
            {
                return Err("the retained resource change did not reach the store".into());
            }
            if hosted.member.is_some() {
                self.follow_member_document(hosted, author, message);
                self.forward_member_document(hosted.workspace_id(), message.clone());
            } else {
                self.follow_write(
                    hosted,
                    author,
                    message.clone(),
                    (app.to_owned(), document.clone()),
                    compact && last,
                )
                .await;
            }
        }
        if hosted.member.is_some() {
            self.wrote(hosted.workspace(), app, document);
        }
        // Kept by the store and the workspace's copy, the write goes out: what a device is
        // served is never a write its own store refused.
        self.inner
            .records
            .carry_all(hosted.workspace_id(), author, messages)?;
        Ok(())
    }

    /// Answer a read of `workspace`'s store, `command`, as `context`'s reader reads it: what an
    /// app's window, or the agent engine, reads of the store it writes in.
    #[cfg(feature = "apps")]
    async fn read_store(
        &self,
        context: &tilo_broker_core::services::ResourceRequestContext,
        workspace: &str,
        command: &collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, String> {
        let reader = person_reading(context.reader());
        self.resource_read(workspace, &reader, command).await
    }

    /// Answer an app's read `command` from the workspace `workspace` holds, with only what
    /// `reader` may read: the store a [`super::readings::Reading`] keeps is read without the
    /// workspace's lock, a command at a time, so a read never waits for a write that waits on
    /// the key service or a link, nor for the index of the record to be brought up to date.
    ///
    /// # Errors
    /// The workspace is not held here, or the store cannot read it.
    pub(crate) async fn resource_read(
        &self,
        workspace: &str,
        reader: &str,
        command: &collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, String> {
        // Who is in the workspace is its governance's to say, never a record an app keeps
        // beside it: its names, roles, join times and pictures are read here, where the
        // workspace's profiles and its record of its agents are.
        if is_workspace_members_read(command) {
            let reading = self
                .reading_of(workspace)
                .await
                .ok_or_else(|| "this server does not hold that workspace".to_owned())?;
            let peer = reading.replica.read(tilo_peer_state::StatePeer::clone);
            return Ok(collab_types::store::wire::Reply::Content(Some(
                workspace_members(self, &peer, &agents_named(&reading, workspace))?,
            )));
        }
        if let Some(answer) = self.read_record_now(workspace, reader, command) {
            return answer;
        }
        // A workspace whose reading is not kept here reads as it opens: it waits for the
        // workspace's lock once.
        let reading = self
            .reading_of(workspace)
            .await
            .ok_or_else(|| "this server does not hold that workspace".to_owned())?;
        read_from(&reading, reader, command)
    }

    /// Answer `command`, a read of `workspace`'s record as `reader` reads it, from the store the
    /// workspace is read through, without its lock: `None` when this server does not know what it
    /// is read through yet, and the read waits for the workspace once
    /// ([`Self::resource_read`]). A record's reads answer here: no reactor, and no thread of its
    /// own.
    pub(crate) fn read_record_now(
        &self,
        workspace: &str,
        reader: &str,
        command: &collab_types::store::wire::Command,
    ) -> Option<Result<collab_types::store::wire::Reply, String>> {
        if is_workspace_members_read(command) {
            // Read from the governance and the profiles, which a held reading alone does not
            // answer: [`Self::resource_read`] does.
            return None;
        }
        self.inner
            .readings
            .held(workspace)
            .map(|reading| read_from(&reading, reader, command))
    }

    /// The resources the store document `document` of `workspace` holds now, by kind and id:
    /// what search reads of one document at a time, from the store the workspace is read
    /// through, without its lock. `None` when this server does not know what the workspace is
    /// read through yet.
    pub(crate) fn held_resources(
        &self,
        workspace: &str,
        document: &str,
    ) -> Option<Vec<(String, String)>> {
        use tilo_lock::MutexRecover as _;
        let reading = self.inner.readings.held(workspace)?;
        let store = reading.store.lock_recover();
        Some(
            store
                .index()
                .held(document)
                .map(|(kind, id)| (kind.to_owned(), id.to_owned()))
                .collect(),
        )
    }
}

/// Answer a read `command` of an app's record from `reading`'s store, with only what `reader`
/// may read: the one store the workspace holds, without the workspace's lock.
fn read_from(
    reading: &super::readings::Reading,
    reader: &str,
    command: &collab_types::store::wire::Command,
) -> Result<collab_types::store::wire::Reply, String> {
    use collab_types::store::wire::{Command, Reply};
    use tilo_lock::MutexRecover as _;
    use tilo_resource_store::wire::read;
    let may_read = may_read_in(reading, reader);
    let mut store = reading.store.lock_recover();
    let answer = match command {
        Command::Versions { kind, id } if store.get(kind, id, &may_read).is_some() => {
            Reply::Versions(
                store
                    .read_versions(kind, id)
                    .map_err(|error| error.to_string())?,
            )
        }
        Command::VersionEnd { kind, id, last } if store.get(kind, id, &may_read).is_some() => {
            Reply::Versions(
                store
                    .read_version_end(kind, id, *last)
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .collect(),
            )
        }
        // A record the caller may not read has no versions for them.
        Command::Versions { .. } | Command::VersionEnd { .. } => Reply::Versions(Vec::new()),
        // The caller's last steps still in effect, read from the documents' change graph.
        Command::Steps {
            steps,
            invoker,
            kinds,
            redo,
        } => tilo_resource_store::wire::steps(&mut store, *steps, invoker, kinds, *redo),
        Command::ChildrenAt {
            parent,
            change,
            from,
        } => Reply::ChildrenAt(
            store
                .read_children_at(parent, change, &may_read)
                .map_err(|error| error.to_string())?
                .map(|children| collab_types::store::wire::PastChildren::page(children, *from)),
        ),
        // A review reads the documents of what it restores, wherever they are kept.
        Command::ReviewRestore { resources, change } => {
            let readable = tilo_resource_store::wire::readable(&store, resources, &may_read);
            Reply::Review(
                store
                    .review_restore(&readable, change)
                    .map_err(|error| error.to_string()),
            )
        }
        _ => read(&store, command, &may_read).unwrap_or(Reply::Written),
    };
    Ok(answer)
}

/// The one place a person holds in the workspace, an owner over an administrator and a role it
/// gives beneath them.
pub(super) fn place(owner: bool, administrator: bool, role: Option<&str>) -> Option<&str> {
    if owner {
        Some("owner")
    } else if administrator {
        Some("administrator")
    } else {
        role
    }
}

/// The workspace's members as its governance lists them, one row each: their actor, what it calls
/// them, their role, and whether they own or administer it. The host's own person is listed like
/// anyone: on a person's computer they are its person, and an owner who hands the workspace on
/// stays in it as an administrator.
pub(super) fn member_rows(
    peer: &tilo_peer_state::StatePeer,
) -> Result<Vec<serde_json::Value>, String> {
    let owner = peer.owner();
    let list = peer.governance().member_list(tilo_clock::wall())?;
    Ok(list
        .members
        .iter()
        .filter(|member| !member.removed)
        .map(|member| {
            let owns = member.root().is_some_and(|root| root == owner);
            let role = member.role.map(tilo_peer_state::join::Role::key);
            serde_json::json!({
                "member": member.id,
                "name": (!member.name.is_empty()).then_some(&member.name),
                "role": role,
                "owner": owns,
                "administrator": member.administrator,
                "place": place(owns, member.administrator, role),
            })
        })
        .collect())
}

/// The agents that work in `workspace`, as its record of its agents names them, for the members'
/// resource: read from the store `reading` shares, never through the workspace's lock. An app
/// reads the members' resource while it answers a call, and whatever waits on that call may have
/// the workspace locked: a read that took the lock would wait for itself.
fn agents_named(reading: &super::readings::Reading, workspace: &str) -> Vec<AgentRow> {
    use tilo_lock::MutexRecover as _;
    let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let bytes = reading
        .store
        .lock_recover()
        .content_document(tilo_workspace_agent::KIND, &list)
        .ok()
        .flatten()
        .map(automerge::Automerge::save);
    let Some(bytes) = bytes else {
        return Vec::new();
    };
    let Ok(agents) = super::actions::agents::read_resource_bytes::<
        tilo_workspace_agent::WorkspaceAgents,
    >((workspace, &list), &bytes) else {
        return Vec::new();
    };
    agents
        .active()
        .map(|agent| AgentRow {
            id: agent.id.0.as_str().to_owned(),
            name: agent.name.clone(),
            avatar: agent.avatar.clone(),
            acts_for: agent.acts_for.clone(),
        })
        .collect()
}

/// Whether `command` reads the workspace's members' resource, which its governance answers.
fn is_workspace_members_read(command: &collab_types::store::wire::Command) -> bool {
    matches!(
        command,
        collab_types::store::wire::Command::Content { kind, id }
            if kind == tilo_resource_store::WORKSPACE_MEMBERS_KIND
                && id == tilo_resource_store::WORKSPACE_MEMBERS_ID
    )
}

/// The workspace's members' resource as its governance has them: its owner and its members,
/// none who are blocked (they hold nothing), then the agents its grants admit and the bots they
/// installed. Each row carries what a view shows of them — the name the workspace keeps, their
/// role, when they joined and the picture they set — so every surface that shows people reads
/// them here, through the store.
fn workspace_members(
    workspaces: &Workspaces,
    peer: &tilo_peer_state::StatePeer,
    agents: &[AgentRow],
) -> Result<serde_json::Value, String> {
    let now = tilo_clock::unix_now_ns() / 1_000_000_000;
    let policy = peer.governance().member_policy()?;
    let listed = peer.governance().member_list(tilo_clock::wall())?;
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for member in &listed.members {
        let Some(root) = member.root() else {
            continue;
        };
        let person = hex::encode(root);
        if !member.removed && policy.is_blocked(&person) {
            continue;
        }
        let who = tilo_actions::Actor::Member(tilo_actions::MemberId::Person(
            tilo_actions::PersonId::from(person.as_str()),
        ));
        let mut row = serde_json::Map::new();
        row.insert(
            "actor".into(),
            serde_json::to_value(&who).map_err(|error| error.to_string())?,
        );
        row.insert("name".into(), serde_json::json!(member.name));
        row.insert("removed".into(), serde_json::json!(member.removed));
        if let Some(role) = member.role {
            row.insert(
                "role".into(),
                serde_json::to_value(role).map_err(|error| error.to_string())?,
            );
        }
        if let Some(joined) = policy.joined_at(&person) {
            row.insert("joined_at".into(), serde_json::json!(joined));
        }
        if let Some(photo) = workspaces
            .person_profile_photo(&root)
            .and_then(|bytes| collab_types::photo_url(&bytes))
        {
            row.insert("photo".into(), serde_json::json!(photo));
        }
        rows.push(serde_json::Value::Object(row));
    }
    for (agent, _) in policy.agent_members_at(now) {
        let id = agent.id().to_owned();
        let acts_for = agent
            .as_agent()
            .map_or_else(String::new, |(_, person)| person.as_str().to_owned());
        let named = agents
            .iter()
            .find(|named| named.id == id && named.acts_for == acts_for);
        let mut row = serde_json::Map::new();
        row.insert(
            "actor".into(),
            serde_json::to_value(tilo_actions::Actor::Member(agent.clone()))
                .map_err(|error| error.to_string())?,
        );
        row.insert(
            "name".into(),
            serde_json::json!(named.map_or_else(|| id.clone(), |named| named.name.clone())),
        );
        row.insert("removed".into(), serde_json::json!(false));
        if let Some(photo) = named
            .and_then(|named| named.avatar.as_deref())
            .and_then(collab_types::avatar_of)
            .and_then(|avatar| collab_types::photo_url(&avatar.0))
        {
            row.insert("photo".into(), serde_json::json!(photo));
        }
        rows.push(serde_json::Value::Object(row));
    }
    // The workspace's installed bots: the grants it made them name them, and a bot reads what
    // they allow it through the host that runs it.
    let mut bots: Vec<String> = policy
        .grants
        .iter()
        .filter_map(|grant| match &grant.grantee {
            tilo_action_policy::Grantee::Member(tilo_action_policy::MemberId::Bot(bot)) => {
                Some(bot.clone())
            }
            _ => None,
        })
        .collect();
    bots.sort_unstable();
    bots.dedup();
    for bot in bots {
        let who = tilo_actions::Actor::Member(tilo_action_policy::MemberId::Bot(bot.clone()));
        rows.push(serde_json::json!({
            "actor": serde_json::to_value(&who).map_err(|error| error.to_string())?,
            "name": bot,
            "removed": false,
        }));
    }
    Ok(serde_json::json!({ "people": rows }))
}

/// An agent as the workspace's record of its agents names it, for the members' resource.
pub(crate) struct AgentRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) avatar: Option<String>,
    pub(crate) acts_for: String,
}

impl Workspaces {
    /// Commit `command` as an update of the fields of a resource every member reads that stays
    /// where it is, when it is one: `false` for any other command, which is drafted whole.
    async fn put_in_place(
        &self,
        hosted: &mut HostedWorkspace,
        device: &super::MemberDevice,
        app: &str,
        command: &collab_types::store::wire::Command,
    ) -> Result<bool, String> {
        let collab_types::store::wire::Command::Put {
            kind,
            id,
            resource,
            stamp,
        } = command
        else {
            return Ok(false);
        };
        let in_place = hosted.member.is_none()
            && resource.readers == collab_types::EVERY_MEMBER
            && hosted
                .resources
                .store_mut()
                .index()
                .get(kind, id)
                .is_some_and(|held| {
                    held.parent == resource.parent
                        && held.order == resource.order
                        && collab_types::store_document_readers(&held.doc)
                            == Some(collab_types::EVERY_MEMBER)
                });
        if !in_place {
            return Ok(false);
        }
        let leave = if stamp.verb == collab_types::Stamp::ORIGIN_VERB {
            use tilo_lock::MutexRecover as _;
            // The record stands where the store holds it: its place, and each record above it,
            // are what the origin's write is held to.
            let store = hosted.resources.store.lock_recover();
            let mut places = std::collections::HashMap::new();
            chain_places(&store, &mut places, kind, id);
            let ground = OriginGround {
                places: std::sync::Arc::new(places),
                origins: std::sync::Arc::new(declared_origins(&store)),
            };
            drop(store);
            hosted.origin_leave(device.device(), std::slice::from_ref(stamp), true, &ground)
        } else {
            None
        };
        let admit = |proposal: &tilo_resource_store::Proposal<'_>| {
            proposal
                .stamp
                .is_none_or(|stamp| stamp.verb != collab_types::Stamp::ORIGIN_VERB)
                || leave.as_ref().is_some_and(|leave| leave.admits(proposal))
        };
        let write = self.store_of(&mut *hosted).draft_field_update(
            (kind, id),
            resource,
            stamp,
            device.device(),
            None,
            &admit,
        )?;
        if let Some(write) = write {
            self.commit_resource_update(hosted, device, app, &write, |_| true)
                .await?;
        }
        Ok(true)
    }

    /// Carries out an app's command on its certified route. A write requires the caller's
    /// matching local signer, and a read returns only what this device's person may read.
    /// A batch the store refuses a step of writes nothing.
    ///
    /// # Errors
    /// The workspace is not held here, the store refused the write, or the workspace refused a
    /// document's write.
    #[cfg(feature = "apps")]
    pub async fn resource_command(
        &self,
        context: &tilo_broker_core::services::ResourceRequestContext,
        command: collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, String> {
        self.answer_resource_command(context, command).await
    }

    #[cfg(feature = "apps")]
    async fn answer_resource_command(
        &self,
        context: &tilo_broker_core::services::ResourceRequestContext,
        mut command: collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, String> {
        let workspace = context.workspace_id();
        if let collab_types::store::wire::Command::Turn {
            address,
            inputs,
            answer,
        } = command
        {
            return self
                .once_turn(workspace, context.app_id(), address, inputs, answer)
                .await
                .map(collab_types::store::wire::Reply::Turn);
        }
        if let collab_types::store::wire::Command::Reach { record } = &command {
            return Ok(collab_types::store::wire::Reply::Reach(self.record_reach(
                workspace,
                context.app_id(),
                record,
            )));
        }
        let opened = || {
            context
                .record()
                .ok_or("this route opened no record to change")
        };
        if let collab_types::store::wire::Command::RecordChanges { after, from, .. } = &command {
            return self
                .opened_record_changes(
                    workspace,
                    (context.app_id(), context.kind(), opened()?),
                    after,
                    *from,
                )
                .await;
        }
        if matches!(command, collab_types::store::wire::Command::Records) {
            return self
                .app_records(workspace, context.app_id())
                .await
                .map(collab_types::store::wire::Reply::Ids);
        }
        // The write is this device's, not a member copy a plain service does not keep: the
        // same device `record_command` signs as.
        let device = self.signing_device();
        let writing = command.writes();
        // Who is in the workspace changes through its governance's own member actions, which the
        // members' resource is read from: an app's write of it would be a second list.
        if writing && writes_kind(&command, tilo_resource_store::WORKSPACE_MEMBERS_KIND) {
            return Err("the workspace's members change through its member actions".into());
        }
        let (acting, unserved) =
            stamp_resource_write(context, workspace, &mut command, &device, writing)?;
        if let collab_types::store::wire::Command::RecordChange {
            changes,
            more,
            commit,
            ..
        } = &command
        {
            return self
                .record_change_command(
                    context,
                    workspace,
                    (context.app_id(), context.kind(), opened()?),
                    (&changes.0, commit.as_ref()),
                    *more,
                    acting.as_ref(),
                )
                .await;
        }
        if !writing {
            return self.read_store(context, workspace, &command).await;
        }
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let hosted = state.lock_owned().await;
        if let Some(viewer) = context.viewer() {
            may_write_as_viewer(&hosted, viewer, context.app_id(), &unserved)?;
        }
        let by_app = app_records::written_by_app(&command, context.app_id());
        match self
            .write_resources(hosted, device, context.app_id(), command)
            .await
        {
            Err(error) if by_app => Err(self
                .app_write_refused(workspace, context.app_id(), error)
                .await),
            written => written,
        }
    }

    /// Carry out the record change `command` names, saying a refusal of the app's own change as
    /// the app's.
    #[cfg(feature = "apps")]
    async fn record_change_command(
        &self,
        context: &tilo_broker_core::services::ResourceRequestContext,
        workspace: &str,
        (app, kind, record): (&str, Option<&str>, &str),
        (changes, commit): (&[u8], Option<&collab_types::WorkspaceCommit>),
        more: bool,
        acting: Option<&tilo_actions::Actor>,
    ) -> Result<collab_types::store::wire::Reply, String> {
        // The broker gathers a write's runs and hands the store the write whole.
        if more {
            return Err("the record's changes reach this device only as a whole write".into());
        }
        // Whether a change is the app's own, made with no person at it: a refusal of one is
        // said as the app's ([`Self::app_write_refused`]).
        let by_app = std::sync::atomic::AtomicBool::new(false);
        // A change's label names whoever made it: the caller writing, or the invoker of an
        // invocation the app serves for them. One naming nobody is its signer's own.
        let names_caller = |stamp: &tilo_resource_store::Stamp| {
            if app_records::by_app(stamp, app) {
                by_app.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            match stamp.invoked_by() {
                None => true,
                Some(named) => {
                    acting == Some(&named)
                        || context
                            .invoker_of(&stamp.invocation)
                            .is_some_and(|invoker| invoker.by == named)
                }
            }
        };
        let written = self
            .opened_record_change(
                workspace,
                (app, kind, record),
                (changes, commit),
                &names_caller,
                context.founding_refusal(),
            )
            .await;
        if let Err(error) = written {
            return Err(if by_app.load(std::sync::atomic::Ordering::Relaxed) {
                self.app_write_refused(workspace, app, error).await
            } else {
                error
            });
        }
        Ok(collab_types::store::wire::Reply::Written)
    }

    /// Carry out `command` on `app`'s record of `workspace` for this device's own roles (the agent
    /// engine writing a reply, a record served where no window of its app is open), as the
    /// device this service signs its own writes as: a read answers what `reader` may read, and each
    /// write is stamped with the invoker the command names before it reaches the
    /// workspace ([`crate::tl_records`]).
    ///
    /// # Errors
    /// [`RecordFailure::Refused`] when the store or the workspace refused a write, with why;
    /// [`RecordFailure::Failed`] when the workspace is not held here.
    #[cfg(any(feature = "apps", feature = "agents"))]
    pub async fn record_command(
        &self,
        workspace: &str,
        app: &str,
        reader: &str,
        command: collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, RecordFailure> {
        let device = self.signing_device();
        if !command.writes() {
            return self
                .resource_read(workspace, reader, &command)
                .await
                .map_err(RecordFailure::Failed);
        }
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| RecordFailure::Failed(error.to_string()))?;
        let hosted = state.lock_owned().await;
        let by_app = app_records::written_by_app(&command, app);
        match self.write_resources(hosted, device, app, command).await {
            Ok(reply) => Ok(reply),
            Err(error) if by_app => Err(RecordFailure::Refused(
                self.app_write_refused(workspace, app, error).await,
            )),
            Err(error) => Err(RecordFailure::Refused(error)),
        }
    }

    /// This device's changes of `app`'s `record` in `workspace` that no other device keeping the
    /// workspace has taken in yet, and those the workspace refused, as this device's sync keeps
    /// them ([`tilo_peer_state::reach`]): what the app's views read as `reach(r)`. An app asks
    /// only of its own records.
    fn record_reach(
        &self,
        workspace: &str,
        app: &str,
        record: &str,
    ) -> collab_types::store::wire::Reach {
        let Some(workspace) = tilo_server_protocol::WorkspaceId::parse(workspace) else {
            return collab_types::store::wire::Reach::default();
        };
        let reach = self.inner.store.record_reach(workspace, app, record);
        let changes = |writes: Vec<tilo_peer_state::reach::OwnWrite>| {
            writes
                .into_iter()
                .flat_map(|write| write.changes)
                .map(hex::encode)
                .collect()
        };
        collab_types::store::wire::Reach {
            pending: changes(reach.pending),
            refused: changes(reach.refused),
        }
    }

    /// Each record of a workspace here whose reach moved: one of this device's writes of it was
    /// sent, taken in by another device that keeps the workspace, or refused. What tells an
    /// app's windows to read `reach(r)` again (the app host's `follow_reach`).
    #[must_use]
    pub fn reach_moved(
        &self,
    ) -> tokio::sync::broadcast::Receiver<tilo_peer_state::reach::ReachMoved> {
        self.inner.store.reach_moved()
    }

    /// Whether this device runs `app`'s action `address` on `inputs`, which runs once in
    /// `workspace` ([`crate::work::Turn::Once`]); with `done`, keep that it ran and let the turn
    /// go. Asking again while this device runs it renews its claim.
    pub(crate) async fn once_turn(
        &self,
        workspace: &str,
        app: &str,
        address: String,
        inputs: serde_json::Value,
        answer: Option<serde_json::Value>,
    ) -> Result<collab_types::store::wire::Turn, String> {
        use crate::work::{NotTaken, Turn};
        use collab_types::store::wire::Turn as Answer;
        if address.split_once('/').map(|(of, _)| of) != Some(app) {
            return Err(format!(
                "{app} runs only its own actions once, not {address}"
            ));
        }
        let turn = Turn::once(address, inputs);
        if let Some(answer) = answer {
            crate::work::keep_done(self, workspace, &turn, answer).await?;
            self.write_lease(workspace, &turn.lease(), 0).await?;
            let kept = crate::work::turn_done(self, workspace, &turn).await;
            return Ok(Answer::Done(kept.unwrap_or_default()));
        }
        Ok(
            match crate::work::takes_turn(self, workspace, &turn).await {
                Ok(_claim) => Answer::Take,
                Err(NotTaken::Done(answer)) => Answer::Done(answer),
                Err(NotTaken::Ineligible(why)) => {
                    tracing::debug!(%workspace, lease = %turn.lease(), %why, "a once-only action waits for a device that may run it");
                    Answer::Later
                }
                Err(NotTaken::TakenBy(_) | NotTaken::Waiting { .. } | NotTaken::Unwritten(_)) => {
                    Answer::Later
                }
            },
        )
    }

    /// Carry out the write `command` of `app` on the workspace `hosted` holds, as `device`,
    /// holding the workspace until the last of its frames is kept.
    #[cfg(any(feature = "apps", feature = "agents"))]
    async fn write_resources(
        &self,
        mut hosted: tokio::sync::OwnedMutexGuard<HostedWorkspace>,
        device: std::sync::Arc<super::MemberDevice>,
        app: &str,
        command: collab_types::store::wire::Command,
    ) -> Result<collab_types::store::wire::Reply, String> {
        use collab_types::store::wire::Reply;
        // Every write names who made it, the app itself where nobody is at it: one that names no
        // one is signed by nobody here, as it would be decided as this device's person.
        if app_records::names_no_one(&command) {
            return Err(app_records::nameless(app));
        }
        // The store's types and verbs are declared before the first write, not by a read that
        // came before it: a store nobody read yet places a write by what its surfaces name.
        drop(self.store_of(&mut hosted));
        if self
            .put_in_place(&mut hosted, &device, app, &command)
            .await?
        {
            drop(hosted);
            return Ok(Reply::Written);
        }

        // A batch the store refuses a step of writes none of its steps.
        let writes = tilo_resource_store::wire::draft(&mut self.store_of(&mut hosted), command)
            .map_err(|refused| refused.error.to_string())?;
        self.keep_writes(hosted, device, app, writes, None).await
    }

    /// Admit `writes`, drafted against `hosted`'s store, and keep them in order as `device`'s
    /// writes for `app`: the store applies each as it is kept, and every holder is sent it.
    ///
    /// Writes to several documents are one action, which every store takes whole or not at all:
    /// each write carries the action's commit, and a store that stops between two of them takes
    /// none. `action` is the action they are part of when its other writes come apart (an app's
    /// write of several records, one record at a time): the store takes these once the others
    /// are here.
    pub(super) async fn keep_writes(
        &self,
        mut hosted: tokio::sync::OwnedMutexGuard<HostedWorkspace>,
        device: std::sync::Arc<super::MemberDevice>,
        app: &str,
        writes: Vec<tilo_resource_store::Written>,
        action: Option<collab_types::WorkspaceCommit>,
    ) -> Result<collab_types::store::wire::Reply, String> {
        use collab_types::store::wire::Reply;
        let app = app.to_owned();
        // The store's types and verbs are declared before the origin's leave reads them.
        drop(self.store_of(&mut hosted));
        // A batch the store refuses a step of keeps none of its steps: check every write against
        // the store as the writes before it leave it, before a frame of the batch is retained.
        // The origin's write is judged by the same leave a holder judges it by, so a write this
        // device carries for a member is no more than one it would take from them.
        let origin = hosted.origin_leaves(device.device(), &writes, true);
        self.store_of(&mut hosted)
            .check_batch_with(
                &writes,
                device.device(),
                &OriginAdmission { origin: &origin },
            )
            .map_err(|refusal| format!("the resource change was refused: {refusal:?}"))?;
        // The store takes these writes as they are kept, unless the action has others to come.
        let whole = action.is_none();
        let action = action.or_else(|| action_of(&hosted, &device, &app, &writes));
        let mut candidate = hosted.peer().clone();
        let mut documents = BTreeSet::new();
        if writes.iter().any(|write| {
            documents.insert(&write.doc)
                && !write.change.deps().is_empty()
                && !candidate.holds_document(&app, &write.doc)
        }) {
            if hosted.member.is_some() {
                self.recall_member_resources(&mut hosted, None).await?;
            } else {
                self.recall(&mut hosted).await?;
            }
            candidate = hosted.peer().clone();
        }
        let mut requests = Vec::with_capacity(writes.len());
        for write in &writes {
            requests.push(
                self.admit_resource_write(
                    &hosted,
                    &mut candidate,
                    &device,
                    &app,
                    (write, action.as_ref()),
                )
                .await?,
            );
        }

        // A member copy keeps the write here whether or not the device serving it is reached: it
        // goes to that device now, or the moment the link is back, and `served` answers once it
        // holds it.
        let (complete, answer) = tokio::sync::oneshot::channel();
        let workspaces = self.clone();
        // Once retention starts, every write is kept even if the caller goes away: the batch is
        // one action.
        tokio::spawn(async move {
            let result = async {
                let author = device.device();
                let mut messages = Vec::with_capacity(requests.len());
                for (write, envelopes) in writes.iter().zip(&requests) {
                    messages.push(Self::resource_write_messages(
                        hosted.workspace_id(),
                        author,
                        &app,
                        write,
                        envelopes,
                    )?);
                }
                hosted.resources.batch();
                // Each write is kept before a frame of it is carried: one the store or the
                // workspace refuses leaves no bytes on the wire, under the numbers its store's
                // next draft takes again. The batch was checked whole above.
                for (index, (write, messages)) in writes.iter().zip(&messages).enumerate() {
                    workspaces
                        .keep_resource_write(
                            &mut hosted,
                            &device,
                            &app,
                            (write, whole),
                            messages,
                            // Compaction can consume a sequence the batch already drafted.
                            index.saturating_add(1) == requests.len(),
                        )
                        .await?;
                }
                Ok(Reply::Written)
            }
            .await;
            hosted.resources.batch_kept();
            let _ = complete.send(result);
        });
        answer
            .await
            .map_err(|_| "the resource writer stopped".to_owned())?
    }
}

/// The action `writes`, `device`'s for `app`, are when they change more than one document: one
/// part per document, naming the changes of it no other change of the batch builds on. `None`
/// for writes to one document, which its own history keeps whole.
///
/// A write that founds its document is no part: it is drafted as the document's first snapshot,
/// which claims no change, so a reader holding the record sealed could never find it taken in,
/// and every record the action names would wait for it for good. A document nobody holds yet
/// has nothing another record's write could land out of step with, and it is kept in its place
/// in the batch all the same.
fn action_of(
    hosted: &HostedWorkspace,
    device: &super::MemberDevice,
    app: &str,
    writes: &[tilo_resource_store::Written],
) -> Option<collab_types::WorkspaceCommit> {
    let mut parts: Vec<collab_types::CommitPart> = Vec::new();
    for write in writes.iter().filter(|write| {
        !write.change.deps().is_empty() || hosted.peer().holds_document(app, &write.doc)
    }) {
        let hash = write.change.hash().0;
        match parts.iter_mut().find(|part| part.document == write.doc) {
            Some(part) => part.heads.push(hash),
            None => parts.push(collab_types::CommitPart {
                app: app.to_owned(),
                document: write.doc.clone(),
                heads: vec![hash],
            }),
        }
    }
    if parts.len() < 2 {
        return None;
    }
    for part in &mut parts {
        part.heads.retain(|head| {
            !writes.iter().any(|other| {
                other.doc == part.document && other.change.deps().iter().any(|dep| dep.0 == *head)
            })
        });
    }
    let workspace = uuid::Uuid::parse_str(hosted.workspace()).ok()?;
    let at_ms = tilo_clock::wall()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        });
    Some(collab_types::WorkspaceCommit::new(
        workspace,
        collab_types::CommitAuthor {
            actor: None,
            device: device.device(),
        },
        at_ms,
        None,
        None,
        parts,
    ))
}

impl Workspaces {
    /// Name `app`'s `surface`, the `surface.toml` its package ships, where this device's
    /// workspaces judge their resource writes by it: what launching the package does on a device
    /// that runs it, for a test whose app is a package this build does not carry.
    #[cfg(any(test, feature = "test-support"))]
    #[expect(
        clippy::panic,
        reason = "a test's surface that does not compile is the test's bug"
    )]
    pub fn declare_package_surface(&self, app: &str, surface: &[u8]) {
        if let Err(why) = crate::workspaces::keep_surface(&self.inner.host.surfaces, app, surface) {
            panic!("{why}");
        }
    }

    /// `hosted`'s store with every app surface this device knows declared
    /// ([`HostedWorkspace::store_with_surfaces`]), its live resources shown where this service
    /// shows them ([`Self::show_live`]).
    pub(super) fn store_of<'a>(
        &self,
        hosted: &'a mut HostedWorkspace,
    ) -> std::sync::MutexGuard<'a, Store> {
        if let (Some(rows), Ok(device)) = (self.inner.live.get(), self.member_identity()) {
            rows.here_is(device.device());
            hosted
                .resources
                .show_live(rows, hex::encode(device.cert.user_public_key));
        }
        let reader = self.own_reader();
        let policy = hosted
            .peer()
            .permissions(reader, tilo_clock::wall())
            .map(|permissions| std::sync::Arc::clone(&permissions.policy));
        if let Ok(policy) = policy {
            hosted.resources.follow_policy(policy);
        }
        hosted.store_with_surfaces(&self.inner.host.surfaces)
    }

    /// Show the live resources of every workspace held here in `rows` from now on, as this
    /// device's person may read them: what the shell shows in the bar, quick settings and on the
    /// lock screen. Each workspace's store says what of it is live as it opens.
    pub async fn show_live(&self, rows: crate::live::LiveRows) {
        if self.inner.live.set(rows).is_err() {
            return;
        }
        for workspace in self.held_workspaces() {
            let Ok(state) = self.workspace(&workspace).await else {
                continue;
            };
            let mut hosted = state.lock().await;
            drop(self.store_of(&mut hosted));
        }
    }
}

impl Workspaces {
    /// [`Self::resource_command`] as this device's own person, for an app the device starts
    /// itself rather than through the broker: the caller is the device the service signs as. A
    /// write serving one of the invocations `serving` names is its invoker's, an agent acting for
    /// the person included.
    ///
    /// # Errors
    /// As [`Self::resource_command`], or this service serves no device.
    #[cfg(feature = "apps")]
    pub async fn own_resource_command(
        &self,
        workspace: &str,
        app: &str,
        command: collab_types::store::wire::Command,
        serving: Vec<(tilo_actions::InvocationId, tilo_actions::Acting)>,
    ) -> Result<collab_types::store::wire::Reply, String> {
        self.own_kind_command(workspace, (app, None), command, serving)
            .await
    }

    /// [`Self::own_resource_command`] on `app`'s records of `kind`, as a route naming the kind
    /// asks: a record the command writes first is founded as one of them.
    ///
    /// # Errors
    /// As [`Self::own_resource_command`], or the command names one of the verbs the platform
    /// writes under, which no app writes under.
    #[cfg(feature = "apps")]
    pub async fn own_kind_command(
        &self,
        workspace: &str,
        (app, kind): (&str, Option<&str>),
        command: collab_types::store::wire::Command,
        serving: Vec<(tilo_actions::InvocationId, tilo_actions::Acting)>,
    ) -> Result<collab_types::store::wire::Reply, String> {
        if let Some(verb) = platform_verb_of(&command) {
            return Err(format!(
                "`{verb}` is the platform's own verb: no app writes under it"
            ));
        }
        let device = self.member_identity()?;
        let caller = tilo_actions::remote::Caller::authenticated(
            device.cert.user_public_key,
            device.device(),
        )
        .in_workspace(workspace);
        let mut context = tilo_broker_core::services::ResourceRequestContext::authenticated(
            app, workspace, caller,
        )
        .serving(serving)
        .of_kind(kind.map(str::to_owned));
        // The device's own reads and writes of an app's record name the record they are of.
        if let collab_types::store::wire::Command::RecordChanges {
            record: Some(record),
            ..
        }
        | collab_types::store::wire::Command::RecordChange {
            record: Some(record),
            ..
        } = &command
        {
            context = context.on_record(record.clone());
        }
        self.resource_command(&context, command).await
    }
}

impl Workspaces {
    /// Holds `query` open for the app `context` names, in its workspace: its first page, then
    /// each page a write here or a member's change makes different, as this device's person
    /// may read it.
    ///
    /// # Errors
    /// The workspace is not held here, or this service serves no device.
    #[cfg(feature = "apps")]
    pub async fn open_resources(
        &self,
        context: &tilo_broker_core::services::ResourceRequestContext,
        query: collab_types::store::Query,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<collab_types::store::Page>, String> {
        let reader = person_reading(context.reader());
        let state = self
            .workspace(context.workspace_id())
            .await
            .map_err(|error| error.to_string())?;
        let mut hosted = state.lock().await;
        drop(self.store_of(&mut hosted));
        Ok(hosted.resources.open_query(query, reader))
    }
}

/// Whether the resource change `write` landed in `opened`, and what says it did not: `sealed`
/// makes it the disposition the admission reported (`accepted`), where a document that travels
/// in the clear is looked for in the document itself.
fn resource_write_landed(
    opened: &tilo_peer_state::OpenedDocument,
    sealed: bool,
    accepted: bool,
    write: &tilo_resource_store::Written,
    decided: &str,
    taken: &tilo_peer_state::ReceiveOutcome,
) -> Result<(), String> {
    let landed = if sealed {
        accepted
    } else {
        Automerge::load(&opened.document)
            .map_err(|error| error.to_string())?
            .has_change(&write.change.hash())
    };
    if landed {
        return Ok(());
    }
    // As for a document that opens to nothing: a change the workspace holds back or
    // refuses leaves the document short of it, and only these counters and what the open
    // left out say which.
    let left_out: Vec<String> = opened
        .unmerged
        .iter()
        .map(|write| format!("{}: {:?}", hex::encode(write.author), write.reason))
        .collect();
    Err(format!(
        "the staged resource change did not pass document admission: {decided}, held \
         back {}, waiting for {}, duplicate {}, refused {}, left out {}",
        taken.deferred,
        taken.incomplete.as_deref().unwrap_or("nothing"),
        taken.duplicate,
        taken.rejected,
        if left_out.is_empty() {
            "nothing".to_owned()
        } else {
            left_out.join("; ")
        }
    ))
}

/// What a write the app makes outside an invocation it serves carries beside the command: who
/// it writes as, and the resource types and verbs whose invocation it does not serve.
#[cfg(feature = "apps")]
type Stamped = (Option<tilo_actions::Actor>, Vec<(String, String)>);

/// Stamp the writes `command` carries as this device's, the way `record_command` signs its own:
/// each write's invoker, and who the app writes as outside an invocation it serves. None of it,
/// and nothing to stamp, for a command that writes nothing.
#[cfg(feature = "apps")]
fn stamp_resource_write(
    context: &tilo_broker_core::services::ResourceRequestContext,
    workspace: &str,
    command: &mut collab_types::store::wire::Command,
    device: &super::MemberDevice,
    writing: bool,
) -> Result<Stamped, String> {
    if !writing {
        return Ok((None, Vec::new()));
    }
    let caller = context.caller();
    if caller.workspace() != Some(workspace)
        || caller.user_public_key() != Some(&device.cert.user_public_key)
        || caller.device_public_key() != Some(&device.device())
    {
        return Err("the resource write requires the certified caller's own signer".into());
    }
    let person = caller
        .acting()
        .person_id()
        .ok_or("this resource route has no caller-scoped signer for the write")?;
    // What an app's process writes outside an invocation it serves is its own: a timer, a
    // background task or a trigger, with nobody at it, is decided by the app's own grants. Only
    // an invocation delivered to it makes a write the invoker's; that someone looks at its window
    // lends it nothing. A view the device opened for a caller still writes no more than that
    // caller may ([`may_write_as_viewer`]). The device's own callers (its shell, a runtime of its
    // own) write for its person, or for the caller the device opened the view for.
    let otherwise = if context.serves_app_process() {
        tilo_actions::Acting::by_app(context.app_id()).in_app(context.app_id())
    } else {
        context.viewer().map_or_else(
            || tilo_actions::Acting::person(person),
            |viewer| viewer.acting().clone(),
        )
    };
    let acting = otherwise.by.clone();
    // The device the write is made on is where a resource it makes live is live.
    let written_on = (
        hex::encode(device.device()),
        device.cert.device_label.clone(),
    );
    // What a view's caller writes outside an invocation: each write's resource type and verb.
    let mut unserved = Vec::new();
    stamp_resource_invoker(
        command,
        &written_on,
        &|invocation| context.invoker_of(invocation).cloned(),
        &otherwise,
        person,
        &mut unserved,
    );
    Ok((Some(acting), unserved))
}

/// Whether `command` writes a resource of `kind`, itself or in its batch.
#[cfg(feature = "apps")]
fn writes_kind(command: &collab_types::store::wire::Command, kind: &str) -> bool {
    use collab_types::store::wire::Command;
    match command {
        Command::Put { kind: written, .. }
        | Command::Remove { kind: written, .. }
        | Command::Edit { kind: written, .. }
        | Command::Checkpoint { kind: written, .. } => written == kind,
        Command::Batch(commands) => commands.iter().any(|command| writes_kind(command, kind)),
        _ => false,
    }
}

/// The command names the verb and invocation, and the workspace service names who made it:
/// whoever invoked the invocation a write serves, when the workspace service delivered that one
/// to the app, and `otherwise` (the app itself on its own process's route; the device's person,
/// or the caller its view was opened for, on the device's own) for any other write, whose
/// resource type and verb are added to `unserved`; the person the route's caller is, `person`;
/// and the device it was written on, `device` (its key in hex and what it calls itself), which is
/// where a resource the write makes live is live.
fn stamp_resource_invoker(
    command: &mut collab_types::store::wire::Command,
    device: &(String, String),
    served: &dyn Fn(&str) -> Option<tilo_actions::Acting>,
    otherwise: &tilo_actions::Acting,
    person: &str,
    unserved: &mut Vec<(String, String)>,
) {
    use collab_types::store::wire::Command;
    let (kind, stamp) = match command {
        Command::Put { kind, stamp, .. }
        | Command::Remove { kind, stamp, .. }
        | Command::Edit { kind, stamp, .. }
        | Command::Checkpoint { kind, stamp, .. } => (kind.as_str(), stamp),
        Command::Undo { stamp, .. }
        | Command::UndoMany { stamp, .. }
        | Command::Restore { stamp, .. } => ("", stamp),
        Command::Batch(commands) => {
            for command in commands {
                stamp_resource_invoker(command, device, served, otherwise, person, unserved);
            }
            return;
        }
        Command::Get { .. }
        | Command::Query(_)
        | Command::Count { .. }
        | Command::Content { .. }
        | Command::Versions { .. }
        | Command::VersionEnd { .. }
        | Command::At { .. }
        | Command::Grouped { .. }
        | Command::Removed { .. }
        | Command::MadeFrom { .. }
        | Command::Newest { .. }
        | Command::Positions { .. }
        | Command::PageAt { .. }
        | Command::Contents { .. }
        | Command::Fields { .. }
        | Command::Values { .. }
        | Command::ChildrenAt { .. }
        | Command::Turn { .. }
        | Command::Reach { .. }
        | Command::RecordChanges { .. }
        | Command::RecordChange { .. }
        | Command::ReviewRestore { .. }
        | Command::Steps { .. }
        | Command::Records => return,
    };
    let mut acting = served(&stamp.invocation).unwrap_or_else(|| {
        unserved.push((kind.to_owned(), stamp.verb.clone()));
        otherwise.clone()
    });
    // An agent works for a person: the person the route's caller is, where its call named none.
    if acting.agent_id().is_some() && acting.acted_for_person().is_none() {
        acting = acting.acting_for(tilo_actions::Acting::person(person));
    }
    if stamp.acted_for.is_none() {
        stamp.acted_for = acting.acted_for_person().map(str::to_owned);
    }
    stamp.invoker = tilo_resource_store::Stamp::by(&acting.by);
    // How an agent acted is recorded, never left to be guessed: handed its session by its
    // person's device, which the device answers for, or under a key of its own, whose call the
    // write must keep.
    if acting.agent_id().is_some() {
        stamp.context.get_or_insert_default().invoker =
            Some(tilo_actions::ActionInvoker::admitted(&acting));
    }
    device.0.clone_into(&mut stamp.device);
    device.1.clone_into(&mut stamp.device_name);
}

/// Who a caller reads records as: a member as the person they are or act for, and a visitor, who
/// is in no resource's reader set, as nobody ([`may_read`]).
pub fn person_reading(caller: &tilo_actions::remote::Caller) -> String {
    let acting = caller.acting();
    acting
        .by
        .member_id()
        .and_then(tilo_actions::MemberId::person_id)
        .or_else(|| acting.acted_for_person())
        .map(ToString::to_string)
        .unwrap_or_default()
}

/// Refuse the writes `unserved` (each a resource type and the verb the app named it by) that the
/// caller `viewer`, whom the device opened this view of `app` for, may not make: a view runs
/// with its viewer's permissions, never the device's.
fn may_write_as_viewer(
    hosted: &HostedWorkspace,
    viewer: &tilo_actions::remote::Caller,
    app: &str,
    unserved: &[(String, String)],
) -> Result<(), String> {
    caller_may(hosted, viewer, app, unserved)
        .map_err(|verb| format!("the view's caller may not {verb} here: the write was refused"))
}

/// Refuse the writes `unserved` (each a resource type and verb of `app`) that `viewer` may not
/// make in `hosted`'s workspace, as its grants decide: the verb refused.
fn caller_may(
    hosted: &HostedWorkspace,
    viewer: &tilo_actions::remote::Caller,
    app: &str,
    unserved: &[(String, String)],
) -> Result<(), String> {
    if unserved.is_empty() {
        return Ok(());
    }
    let workspace = hosted.workspace();
    let policy = hosted.peer().governance().member_policy()?;
    let acting = viewer.acting();
    let mut granted = acting.handed_preset(workspace);
    granted.extend(viewer.granted());
    let authority = tilo_action_policy::Authority::of_acting(acting, &policy, &granted);
    let cx = tilo_action_policy::Context::at_time(tilo_clock::wall());
    for (kind, verb) in unserved {
        let mut node = tilo_action_policy::TreePath::workspace(workspace).app(app);
        if !kind.is_empty() {
            node = node.resource(kind.as_str());
        }
        let node = node.action(verb.as_str());
        let decided = authority.decide(&node, tilo_action_policy::ActionKind::Undoable, &cx);
        if decided.answer != tilo_action_policy::Answer::Allow {
            return Err(verb.clone());
        }
    }
    Ok(())
}

/// The platform's own verb `command` writes under, when it names one: the admission a verb of
/// that name gives (any field of a declared kind) is the platform's alone, and no app writes
/// under one, whatever action it gave the name to.
#[cfg(feature = "apps")]
fn platform_verb_of(command: &collab_types::store::wire::Command) -> Option<&str> {
    use collab_types::store::wire::Command;
    fn named(stamp: &tilo_resource_store::Stamp) -> Option<&str> {
        tilo_resource_store::PLATFORM_VERBS
            .contains(&stamp.verb.as_str())
            .then_some(stamp.verb.as_str())
    }
    match command {
        Command::Put { stamp, .. }
        | Command::Remove { stamp, .. }
        | Command::Undo { stamp, .. }
        | Command::Edit { stamp, .. }
        | Command::Checkpoint { stamp, .. }
        | Command::Restore { stamp, .. }
        | Command::UndoMany { stamp, .. } => named(stamp),
        Command::Batch(commands) => commands.iter().find_map(platform_verb_of),
        _ => None,
    }
}

/// The person `person`, a member's id, as the key the workspace knows them by.
fn person_bytes(person: &str) -> Option<[u8; 32]> {
    match hex::decode(person) {
        Ok(bytes) => {
            if let Ok(person) = bytes.try_into() {
                Some(person)
            } else {
                tracing::warn!(person, "a member's person id is not 32 bytes");
                None
            }
        }
        Err(error) => {
            tracing::warn!(person, %error, "a member's person id is not hex");
            None
        }
    }
}

/// Whether the author of `stamp`, a change under one of the platform's own verbs or one of the
/// history marks, is the device or person the change belongs to: the change names its invoker
/// (or who it acted for), and that person is the author device's own, or the change carries
/// their signed call. A change under none of those verbs belongs to whoever wrote it.
fn platform_write_belongs(
    stamp: Option<&tilo_resource_store::Stamp>,
    author_person: Option<[u8; 32]>,
) -> bool {
    let Some(stamp) = stamp else {
        return true;
    };
    // The platform's own verbs write any field and content of a declared kind, and the history
    // marks write nothing at all; both are the platform's, and their invoker is their author.
    if !tilo_resource_store::PLATFORM_VERBS.contains(&stamp.verb.as_str())
        && !tilo_resource_store::HISTORY_VERBS.contains(&stamp.verb.as_str())
    {
        return true;
    }
    let acting = stamp.invoked_by().map(tilo_action_policy::Acting::from);
    let author = author_person.map(hex::encode);
    let named = author.as_deref().is_some_and(|author| {
        acting
            .as_ref()
            .is_some_and(|acting| acting.people().contains(&author))
            || stamp.acted_for.as_deref() == Some(author)
    });
    if named {
        return true;
    }
    match (acting, stamp.signed.as_deref()) {
        (Some(acting), Some(call)) => {
            tilo_server_protocol::invocation::signs_for(&acting, &call.signed)
        }
        _ => false,
    }
}

/// Whether `proposal`, judged in the batch whose origin's leave is `origin`, may be made: an
/// origin's write is the leave's to admit, and every other write is the caller's own to judge.
fn admits_origin(
    origin: &std::collections::HashMap<String, OriginLeave>,
    proposal: &tilo_resource_store::Proposal<'_>,
) -> bool {
    if proposal
        .stamp
        .is_some_and(|stamp| stamp.verb == collab_types::Stamp::ORIGIN_VERB)
    {
        origin
            .get(proposal.doc)
            .is_some_and(|leave| leave.admits(proposal))
    } else {
        true
    }
}

/// [`admits_origin`] as the store asks it: one batch's origin's leaves.
struct OriginAdmission<'a> {
    origin: &'a std::collections::HashMap<String, OriginLeave>,
}

impl tilo_resource_store::Admit for OriginAdmission<'_> {
    fn admit(&self, proposal: &tilo_resource_store::Proposal<'_>) -> bool {
        admits_origin(self.origin, proposal)
    }
}

/// The changes of `changes` that name the origin's write, each with its stamp.
fn origin_stamps(changes: &[AmChange]) -> Vec<(&AmChange, collab_types::Stamp)> {
    changes
        .iter()
        .filter_map(|change| {
            collab_types::Stamp::of(change)
                .filter(|stamp| stamp.verb == collab_types::Stamp::ORIGIN_VERB)
                .map(|stamp| (change, stamp))
        })
        .collect()
}

/// The device a stamp names as the one its change was written on, by its key in hex.
fn device_of(stamp: &collab_types::Stamp) -> Option<[u8; 32]> {
    if stamp.device.len() != 64 {
        return None;
    }
    let mut key = [0_u8; 32];
    hex::decode_to_slice(stamp.device.as_bytes(), &mut key).ok()?;
    Some(key)
}

/// A record as the document holds it: its kind, and the record it is nested in.
struct RecordPlace {
    kind: String,
    parent: Option<String>,
}

/// What every origin's leave of one batch reads: where each record it touches stands, and each
/// record above it, and the outside kind each kind declares. Shared by each document's leave.
struct OriginGround {
    places: std::sync::Arc<std::collections::HashMap<String, RecordPlace>>,
    origins: std::sync::Arc<std::collections::HashMap<String, Option<String>>>,
}

/// What an origin's write in one document is judged by, read once for the write: the access of
/// every device it may be authored by, where each record it touches stands, and the outside kind
/// each kind declares.
struct OriginLeave {
    writers: std::collections::HashMap<[u8; 32], tilo_action_policy::MemberAccess>,
    /// Where each record the write touches stands, and each record above it, by id: read once for
    /// the batch and shared by each document's leave.
    places: std::sync::Arc<std::collections::HashMap<String, RecordPlace>>,
    origins: std::sync::Arc<std::collections::HashMap<String, Option<String>>>,
    /// This device carried the write for the caller it certified, as a command over its own
    /// route: its stamp names the caller, and the device signed nothing of their own.
    carried: bool,
}

impl OriginLeave {
    /// Whether the origin's write `proposal` is one its writer may make: it names the person it
    /// wrote for, the device it names is one of that person's (or an agent they run), the reader
    /// set includes that person, and every record it touches stands in an origin account's place.
    fn admits(&self, proposal: &tilo_resource_store::Proposal<'_>) -> bool {
        let Some(stamp) = proposal.stamp else {
            return false;
        };
        let device = device_of(stamp).unwrap_or(*proposal.author);
        let Some(access) = self.writers.get(&device) else {
            return false;
        };
        // The change names the person it wrote for, or, for an app's own process taking in what
        // its service said, the person whose device holds the sign-in it used.
        let named = stamp
            .invoked_by()
            .and_then(|invoker| invoker.person_id().map(str::to_owned))
            .or_else(|| access.person_id().map(str::to_owned));
        let Some(person) = named else {
            return false;
        };
        // The device named wrote for the person, or, on the device that carried a command for
        // the caller it certified, it is that device's own write of what they asked for. That
        // is deliberately as far as this goes: a person's own device, or a device whose policy
        // lets it read a shared account's password, could sign in to that account directly, so
        // writing its data as the origin gives it nothing more. Another member's device is
        // refused.
        let named_person = tilo_action_policy::Acting::person(person.as_str());
        if access.invoked_by(&named_person).is_none()
            && !(self.carried && device == *proposal.author)
        {
            return false;
        }
        let Some(key) = person_bytes(&person) else {
            return false;
        };
        let Some(readers) = collab_types::Readers::parse(proposal.readers) else {
            return false;
        };
        // Every member reads a document written for the workspace; a named set reads it where
        // it names the person, by their key or by a role theirs includes.
        if let Some(named) = readers.named()
            && !named.includes(&key, access.policy.role_of(&person))
        {
            return false;
        }
        proposal
            .touched
            .resources
            .iter()
            .all(|(id, touch)| self.in_place(id, touch, proposal.touched))
    }

    /// Whether the record `id` the change `touch` makes stands in an origin account's place: it
    /// is nested, through the records it touches and every record above them, in a record of a
    /// kind an origin keeps (`imap.account`, `caldav.account`).
    fn in_place(
        &self,
        id: &str,
        touch: &collab_types::touch::Touch,
        touched: &collab_types::touch::Touched,
    ) -> bool {
        let mut kind = self
            .places
            .get(id)
            .map(|place| place.kind.clone())
            .or_else(|| touch.kind.clone());
        let mut parent = self
            .places
            .get(id)
            .map_or_else(|| touch.parent.clone(), |place| place.parent.clone());
        for _ in 0..64 {
            if let Some(kind) = &kind
                && self
                    .origins
                    .get(kind)
                    .and_then(Option::as_deref)
                    .is_some_and(|origin| origin.ends_with(".account"))
            {
                return true;
            }
            let Some(next) = parent else {
                return false;
            };
            let made = touched.resources.get(&next);
            kind = self
                .places
                .get(&next)
                .map(|place| place.kind.clone())
                .or_else(|| made.and_then(|touch| touch.kind.clone()));
            parent = self.places.get(&next).map_or_else(
                || made.and_then(|touch| touch.parent.clone()),
                |place| place.parent.clone(),
            );
        }
        false
    }
}

/// The kind each type declares as the outside service it is kept in step with, by kind.
fn declared_origins(store: &Store) -> std::collections::HashMap<String, Option<String>> {
    store
        .schema()
        .types
        .iter()
        .map(|ty| (ty.name.clone(), ty.origin.clone()))
        .collect()
}

/// The place of the record `id`, whichever document holds it: its kind and the record it is
/// nested in.
fn indexed_place(store: &Store, id: &str) -> Option<RecordPlace> {
    let kind = store.index().kind_anywhere(id)?;
    let parent = store
        .index()
        .get(&kind, id)
        .and_then(|row| row.parent.clone());
    Some(RecordPlace { kind, parent })
}

/// Every record's place, from the index alone: what an origin's leave reads where a change cannot
/// be read against the document that holds it.
fn all_places(store: &Store) -> std::collections::HashMap<String, RecordPlace> {
    let docs: Vec<String> = store.index().documents().map(str::to_owned).collect();
    let mut places = std::collections::HashMap::new();
    for doc in docs {
        for (kind, id) in store.index().held(&doc) {
            let parent = store
                .index()
                .get(kind, id)
                .and_then(|row| row.parent.clone());
            places.insert(
                id.to_owned(),
                RecordPlace {
                    kind: kind.to_owned(),
                    parent,
                },
            );
        }
    }
    places
}

/// [`indexed_place`] and each record above it, by id: the chain an origin's write of `id` of
/// `kind` stands in.
fn chain_places(
    store: &Store,
    places: &mut std::collections::HashMap<String, RecordPlace>,
    kind: &str,
    id: &str,
) {
    let mut next = Some((kind.to_owned(), id.to_owned()));
    for _ in 0..64 {
        let Some((kind, id)) = next else {
            break;
        };
        if places.contains_key(&id) {
            break;
        }
        let parent = store
            .index()
            .get(&kind, &id)
            .and_then(|row| row.parent.clone());
        places.insert(
            id.clone(),
            RecordPlace {
                kind,
                parent: parent.clone(),
            },
        );
        next = parent.and_then(|parent| {
            store
                .index()
                .kind_anywhere(&parent)
                .map(|kind| (kind, parent))
        });
    }
}

/// Where each record the `changes` of a document touch stands, and every record above it, by id:
/// read once for an origin's leave. A record's parent is not kept in the document that holds the
/// record (a document is named by the parent its records share), so the chain an origin's record
/// stands in crosses documents, and only the index knows all of it.
fn origin_places<'a>(
    store: &Store,
    changes: impl IntoIterator<Item = (&'a str, &'a AmChange)>,
) -> std::collections::HashMap<String, RecordPlace> {
    // What each record a change touches becomes, by id: its kind and its parent as the change
    // leaves them. A change that only edits or removes one touches its place as the index holds
    // it.
    let mut seeds: std::collections::HashMap<String, (Option<String>, Option<String>)> =
        std::collections::HashMap::new();
    // A document the store does not hold yet, as the changes make it.
    let mut made: std::collections::HashMap<&str, Automerge> = std::collections::HashMap::new();
    for (doc_id, change) in changes {
        let touched = if let Some(doc) = store.document(doc_id) {
            collab_types::touch::touched(doc, doc_id, change)
        } else {
            let doc = made.entry(doc_id).or_default();
            let touched = collab_types::touch::touched(doc, doc_id, change);
            if doc.apply_changes([change.clone()]).is_err() {
                return all_places(store);
            }
            touched
        };
        let Some((touched, _)) = touched else {
            // A change the store cannot read against what it holds: read every place from the
            // index rather than judge a record as standing nowhere.
            return all_places(store);
        };
        for (id, touch) in touched.resources {
            let kind = touch
                .kind
                .clone()
                .or_else(|| store.index().kind_anywhere(&id));
            let parent = if touch.placed {
                touch.parent.clone()
            } else {
                touch.parent.clone().or_else(|| {
                    kind.as_deref()
                        .and_then(|kind| store.index().get(kind, &id))
                        .and_then(|row| row.parent.clone())
                })
            };
            seeds.entry(id).or_insert((kind, parent));
        }
    }
    let mut places = std::collections::HashMap::new();
    let ids: Vec<String> = seeds.keys().cloned().collect();
    for id in ids {
        let mut next = Some(id);
        for _ in 0..64 {
            let Some(id) = next else {
                break;
            };
            if places.contains_key(&id) {
                break;
            }
            // A record the batch makes is placed by the change that makes it; one it only
            // touches, or a record above either, by the index.
            let (kind, parent) = match seeds.get(&id) {
                Some((kind, parent)) => (kind.clone(), parent.clone()),
                None => match indexed_place(store, &id) {
                    Some(place) => (Some(place.kind), place.parent),
                    None => break,
                },
            };
            let Some(kind) = kind else {
                break;
            };
            places.insert(
                id.clone(),
                RecordPlace {
                    kind,
                    parent: parent.clone(),
                },
            );
            next = parent;
        }
    }
    places
}

/// Whether the reader set `readers` names `reader` ([`tilo_resource_store::wire::reads_as`]).
fn may_read(reader: String) -> impl Fn(&str) -> bool {
    move |readers| tilo_resource_store::wire::reads_as(&reader)(readers)
}

/// Whether `reader`, the person this service reads as, reads what the reader set `readers` names,
/// as `reading`'s governance has the workspace now: one of its people, or a member whose role
/// includes the set's ([`collab_types::NamedReaders::includes`]). A set naming a role is read by
/// its holders, never by who held it when the record was written.
pub(super) fn may_read_in(
    reading: &super::readings::Reading,
    reader: &str,
) -> impl Fn(&str) -> bool {
    let peer = reading.replica.read(tilo_peer_state::StatePeer::clone);
    let person = person_bytes(reader);
    let now = tilo_clock::wall();
    move |readers| {
        let (Some(person), Some(set)) = (person, collab_types::Readers::parse(readers)) else {
            return false;
        };
        match set {
            collab_types::Readers::EveryMember => true,
            set => set
                .named()
                .is_some_and(|named| peer.governance().reads_named(&named, &person, now)),
        }
    }
}

#[cfg(any(feature = "apps", feature = "agents"))]
mod app_records;
mod record_changes;
mod verb_grants;

// The tests write and read a device's resources through the store (`apps`) and
// the members around it (`agents`, for the device the sealed fixtures make).
#[cfg(all(test, feature = "apps", feature = "agents"))]
mod tests;

// The origin's write is the platform's own, and no app action makes it: a test of who may
// author one, and where, without a workspace's whole governance around it.
#[cfg(test)]
mod origin_tests {
    use std::collections::BTreeMap;

    use collab_types::touch::{Touch, Touched};
    use tilo_action_policy::{MemberAccess, WorkspacePolicy};
    use tilo_resource_store::{Proposal, Stamp};

    use super::{OriginLeave, RecordPlace, person_bytes, platform_write_belongs};

    const PERSON: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const DEVICE: [u8; 32] = [1; 32];
    const OTHER_DEVICE: [u8; 32] = [2; 32];
    const ACCOUNT: &str = "imap.account";
    const MESSAGE: &str = "imap.message";

    fn leave(writer: &str) -> OriginLeave {
        let policy = WorkspacePolicy::new("workspace", PERSON);
        let access = MemberAccess::person(policy, writer);
        let mut records = std::collections::HashMap::new();
        records.insert(
            "account".to_owned(),
            RecordPlace {
                kind: "tl.mail.Account".to_owned(),
                parent: Some("mail".to_owned()),
            },
        );
        records.insert(
            "message".to_owned(),
            RecordPlace {
                kind: "tl.mail.Message".to_owned(),
                parent: Some("account".to_owned()),
            },
        );
        records.insert(
            "top".to_owned(),
            RecordPlace {
                kind: "tl.mail.Message".to_owned(),
                parent: None,
            },
        );
        OriginLeave {
            writers: std::collections::HashMap::from([(DEVICE, access)]),
            places: std::sync::Arc::new(records),
            origins: std::sync::Arc::new(std::collections::HashMap::from([
                ("tl.mail.Account".to_owned(), Some(ACCOUNT.to_owned())),
                ("tl.mail.Message".to_owned(), Some(MESSAGE.to_owned())),
                ("tl.mail.Mail".to_owned(), None),
            ])),
            carried: false,
        }
    }

    fn touched(id: &str, parent: Option<&str>) -> Touched {
        Touched {
            resources: BTreeMap::from([(
                id.to_owned(),
                Touch {
                    kind: Some("tl.mail.Message".to_owned()),
                    parent: parent.map(str::to_owned),
                    ..Touch::default()
                },
            )]),
            readers: Some(collab_types::EVERY_MEMBER.to_owned()),
        }
    }

    fn stamp(invoker: &str, device: [u8; 32]) -> Stamp {
        Stamp {
            verb: collab_types::Stamp::ORIGIN_VERB.to_owned(),
            invoker: invoker.to_owned(),
            device: hex::encode(device),
            ..Stamp::default()
        }
    }

    fn admits(leave: &OriginLeave, stamp: &Stamp, touched: &Touched, readers: &str) -> bool {
        leave.admits(&Proposal {
            doc: "*/tl.mail#0",
            readers,
            author: &DEVICE,
            stamp: Some(stamp),
            touched,
        })
    }

    #[test]
    fn a_device_writes_the_origin_only_for_its_own_person() {
        let here = leave(PERSON);
        // The person's own device, writing in an account's place, is admitted.
        let own = stamp(PERSON, DEVICE);
        assert!(admits(
            &here,
            &own,
            &touched("message", Some("account")),
            "*"
        ));
        // A change naming another person than the device's is refused, though the document is
        // the same: a member's device writes for nobody else.
        let other = stamp(OTHER, DEVICE);
        assert!(!admits(
            &here,
            &other,
            &touched("message", Some("account")),
            "*"
        ));
        // Nor does another device stand in for this one.
        let elsewhere = stamp(PERSON, OTHER_DEVICE);
        assert!(!admits(
            &here,
            &elsewhere,
            &touched("message", Some("account")),
            "*"
        ));
    }

    #[test]
    fn the_device_that_carried_a_call_admits_it_for_the_caller_it_certified() {
        // A member's command on the workspace's host: the stamp names the person who asked, and
        // the host's own device wrote it. Another holder receiving the same write as that
        // member's own requires their device to stand for them.
        let mut host = leave(PERSON);
        host.carried = true;
        let carried = stamp(OTHER, DEVICE);
        assert!(admits(
            &host,
            &carried,
            &touched("message", Some("account")),
            "*"
        ));
        let remote = leave(PERSON);
        assert!(!admits(
            &remote,
            &carried,
            &touched("message", Some("account")),
            "*"
        ));
    }

    #[test]
    fn a_history_mark_belongs_to_the_person_it_names() {
        // checkpoint, describe and link write nothing, so no kind grant judges them: the mark
        // belongs to the person its stamp names, as a platform verb's write does.
        for verb in ["checkpoint", "describe", "link"] {
            let mut mark = stamp(PERSON, DEVICE);
            mark.verb = verb.to_owned();
            assert!(
                platform_write_belongs(Some(&mark), person_bytes(PERSON)),
                "`{verb}` is its author's own"
            );
            assert!(
                !platform_write_belongs(Some(&mark), person_bytes(OTHER)),
                "`{verb}` naming another person is refused"
            );
            let mut unnamed = mark.clone();
            unnamed.invoker.clear();
            assert!(
                !platform_write_belongs(Some(&unnamed), person_bytes(PERSON)),
                "a `{verb}` naming nobody is refused"
            );
        }
    }

    #[test]
    fn the_origin_writes_only_inside_an_accounts_place_and_to_its_readers() {
        let here = leave(PERSON);
        let own = stamp(PERSON, DEVICE);
        // A record at the top of the workspace stands in no account's place.
        assert!(!admits(&here, &own, &touched("top", None), "*"));
        // Neither does one whose parent the document does not hold.
        assert!(!admits(&here, &own, &touched("fresh", Some("gone")), "*"));
        // A reader set that leaves the person out is no reader set of theirs.
        let only_other = hex::encode([9_u8; 32]);
        assert!(!admits(
            &here,
            &own,
            &touched("message", Some("account")),
            &only_other
        ));
        // The account's own record is in its place.
        let mut account = touched("account", Some("mail"));
        account.resources.get_mut("account").unwrap().kind = Some("tl.mail.Account".to_owned());
        assert!(admits(&here, &own, &account, "*"));
    }

    #[test]
    fn a_change_naming_no_person_is_its_devices_and_an_unknown_device_is_refused() {
        let here = leave(PERSON);
        // A change naming no person is the person of the device it names.
        let no_person = Stamp {
            verb: collab_types::Stamp::ORIGIN_VERB.to_owned(),
            device: hex::encode(DEVICE),
            ..Stamp::default()
        };
        assert!(admits(
            &here,
            &no_person,
            &touched("message", Some("account")),
            "*"
        ));
        // A device the workspace does not know writes nothing.
        let no_device = stamp(PERSON, [3; 32]);
        assert!(!admits(
            &here,
            &no_device,
            &touched("message", Some("account")),
            "*"
        ));
    }
}
