//! A workspace's resources, as the framework's one store keeps them ([`tilo_resource_store`]).
//!
//! Every resource of a workspace is in a document the store placed, one reader set to a
//! document, and a write of one is a member's signed write of that document, sealed for its
//! reader set like any write of an app's document. The governor takes such a write into its store
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
#[cfg(any(feature = "apps", feature = "agents"))]
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
#[cfg(any(feature = "apps", feature = "agents"))]
use tilo_resource_store::Schema;
use tilo_resource_store::{Admit, Received, Refusal, Store};

use super::{GovernedWorkspace, Workspaces};

#[cfg(feature = "agents")]
mod bot_writes;
#[cfg(feature = "agents")]
mod packaged;

/// Why [`Workspaces::record_command`] did not carry a command out.
#[cfg(feature = "agents")]
#[derive(Debug)]
pub enum RecordFailure {
    /// The workspace refused the write: whoever invoked it may not do what it names.
    Refused(String),
    /// It could not be carried out: the workspace is not held here, or the store refused it.
    Failed(String),
}

#[cfg(feature = "agents")]
impl From<String> for RecordFailure {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

/// Where a workspace's store keeps its documents, under the workspace's directory.
const STORE: &str = "store";
const DOCS: &str = "docs";
const INDEX: &str = "index";
/// How long a document nobody wrote stays in memory.
const IDLE: Duration = Duration::from_secs(300);

/// The file a document is kept in: its id, hex-encoded, since an id holds `/`.
fn file_name(doc: &str) -> String {
    hex::encode(doc.as_bytes())
}

/// The document a file keeps.
fn doc_of(file: &std::ffi::OsStr) -> Option<String> {
    tilo_failure::ignored!(
        "a file not named for a document, as one an interrupted write left, is no document",
        String::from_utf8(hex::decode(file.to_str()?).ok()?).ok()
    )
}

/// Writes `bytes` to `path` whole or not at all.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let partial = path.with_extension("partial");
    fs::write(&partial, bytes)?;
    fs::rename(&partial, path)
}

/// One workspace's store, and where it keeps its documents.
pub(super) struct Resources {
    /// Behind a lock of its own: the store reads documents through a source no two threads
    /// share.
    store: std::sync::Mutex<Store>,
    root: PathBuf,
    /// Each document in memory, by when it was last written.
    used: BTreeMap<String, Instant>,
    /// The documents whose kept snapshot is older than they are.
    unsnapshotted: BTreeSet<String>,
    /// How many surfaces the device had named when the store last declared them
    /// ([`collab_types::app_surface::Surfaces::registered`]).
    #[cfg(any(feature = "apps", feature = "agents"))]
    declared: u64,
    /// The queries apps hold open, by the store's watch of their kind.
    #[cfg(feature = "apps")]
    opened: BTreeMap<u64, Opened>,
}

/// A query an app holds open: whenever a change makes what its page shows different, the page
/// is sent again, as the person it is read for may read it.
#[cfg(feature = "apps")]
struct Opened {
    query: tilo_resource_store::Query,
    reader: String,
    pages: tokio::sync::mpsc::UnboundedSender<tilo_resource_store::Page>,
    shown: Vec<(String, tilo_resource_store::Indexed)>,
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
        let mut store = Store::with_source(Box::new(move |doc| {
            fs::read(reading.join(file_name(doc))).ok()
        }));
        let mut used = BTreeMap::new();
        let mut unsnapshotted = BTreeSet::new();
        // What the store kept of each document is read side by side, and each document is
        // indexed from its snapshot: a workspace keeps a chain of parts, each with the
        // documents of its resources, and one read after another made a start wait for all
        // of them.
        let mut files = Vec::new();
        for entry in fs::read_dir(&docs).into_iter().flatten().flatten() {
            if let Some(doc) = doc_of(&entry.file_name()) {
                files.push((doc, entry.path()));
            }
        }
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
            store: std::sync::Mutex::new(store),
            root,
            used,
            unsnapshotted,
            #[cfg(any(feature = "apps", feature = "agents"))]
            declared: 0,
            #[cfg(feature = "apps")]
            opened: BTreeMap::new(),
        }
    }

    /// The store, for the tests of a device's own writes to it.
    #[cfg(all(test, feature = "apps", feature = "agents"))]
    pub(super) fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        use tilo_lock::MutexRecover as _;
        self.store.lock_recover()
    }

    /// The store, while its workspace is held alone.
    fn store_mut(&mut self) -> &mut Store {
        self.store
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The reader set the store knows `doc` by.
    fn readers(&mut self, doc: &str) -> Option<String> {
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
    fn heads(&mut self, doc: &str) -> Vec<ChangeHash> {
        self.store_mut()
            .index()
            .heads(doc)
            .map(<[_]>::to_vec)
            .unwrap_or_default()
    }

    /// The document the store placed under `doc`, saved as it holds it now: from memory, or read
    /// from what it kept when it left memory. `None` when it placed none by that id.
    fn document(&mut self, doc: &str) -> Option<Vec<u8>> {
        if self.store_mut().document(doc).is_none() {
            let bytes = fs::read(self.root.join(DOCS).join(file_name(doc))).ok()?;
            self.store_mut().load(doc, &bytes).ok()?;
        }
        self.used.insert(doc.to_owned(), tilo_clock::now());
        self.store_mut().document(doc).map(Automerge::save)
    }

    /// Keeps what `doc` took since `before`: its changes appended to its file, then the
    /// index's snapshot of it, so a start reads that instead of the document — and a start
    /// after a stop that never ran (a killed process) reads the same.
    fn kept(&mut self, doc: &str, before: &[ChangeHash]) -> Result<(), String> {
        let root = self.root.clone();
        let Some(document) = self.store_mut().document(doc) else {
            return Ok(());
        };
        if document.get_heads() == before {
            return Ok(());
        }
        let docs = root.join(DOCS);
        fs::create_dir_all(&docs).map_err(|error| error.to_string())?;
        let path = docs.join(file_name(doc));
        let snapshot = root.join(INDEX).join(file_name(doc));
        // The snapshot goes before the change it no longer holds: a write killed between
        // them leaves the document whole for the next start to read.
        match fs::remove_file(&snapshot) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        if before.is_empty() || !path.exists() {
            write_whole(&path, &document.save())
        } else {
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .and_then(|mut file| file.write_all(&document.save_after(before)))
        }
        .map_err(|error| error.to_string())?;
        self.used.insert(doc.to_owned(), tilo_clock::now());
        match self.write_snapshot(doc) {
            Ok(()) => {}
            Err(error) => {
                tracing::warn!(%doc, %error, "a resource document's snapshot was not kept");
                self.unsnapshotted.insert(doc.to_owned());
            }
        }
        self.evict_idle();
        #[cfg(feature = "apps")]
        self.tell_opened();
        Ok(())
    }

    /// Holds `query` open as `reader` may read it: its first page now, then each page a change
    /// makes different, until the receiver is dropped.
    #[cfg(feature = "apps")]
    fn open_query(
        &mut self,
        query: tilo_resource_store::Query,
        reader: String,
    ) -> tokio::sync::mpsc::UnboundedReceiver<tilo_resource_store::Page> {
        let (pages, receiver) = tokio::sync::mpsc::unbounded_channel();
        let store = self.store_mut();
        let watch = store.watch(&query.kind);
        let page = store.query(&query, &may_read(reader.clone()));
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
            store.unwatch(watch);
        }
        receiver
    }

    /// Sends each open query whose kind a change touched its page again, when what it shows
    /// changed, and lets go of every query whose app no longer holds it.
    #[cfg(feature = "apps")]
    fn tell_opened(&mut self) {
        if self.opened.is_empty() {
            return;
        }
        let store = self
            .store
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = store.changed();
        self.opened.retain(|watch, open| {
            if open.pages.is_closed() {
                store.unwatch(*watch);
                return false;
            }
            if !changed.contains(watch) {
                return true;
            }
            let page = store.query(&open.query, &may_read(open.reader.clone()));
            if page.resources == open.shown {
                return true;
            }
            open.shown.clone_from(&page.resources);
            let sent = open.pages.send(page).is_ok();
            if !sent {
                store.unwatch(*watch);
            }
            sent
        });
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

impl GovernedWorkspace {
    /// The document the workspace's resource store placed under `doc`, saved as the store holds
    /// it: a document the store keeps, which the governor's replica does not
    /// ([`is_store_document`]). `None` when the store placed none by that id.
    pub(super) fn store_document(&mut self, doc: &str) -> Option<Vec<u8>> {
        self.resources.document(doc)
    }

    /// The workspace's store, with the types and verbs of every app surface the device knows
    /// (`surfaces`) declared: what this device's writes and the members' are judged by.
    #[cfg(any(feature = "apps", feature = "agents"))]
    fn store_with_surfaces(
        &mut self,
        surfaces: &collab_types::app_surface::Surfaces,
    ) -> &mut Store {
        let named = surfaces.registered();
        if self.resources.declared != named {
            self.resources.declared = named;
            for file in surfaces.every() {
                for surface in &file.resources {
                    self.resources
                        .store_mut()
                        .declare(Schema::from_surface(surface));
                }
            }
        }
        self.resources.store_mut()
    }

    /// Takes `author`'s write `envelope` of the store document `doc` of `app` into the
    /// workspace's store, `named` the reader set the write names in the clear. A sealed write is
    /// read with its reader set's keys, under the scope the document's id derives; a device that
    /// holds none keeps and forwards it, as it does any sealed write, and indexes nothing of it.
    ///
    /// # Errors
    /// The write does not open or decode, or the store cannot keep it.
    ///
    /// Whether any change of it took effect.
    pub(super) async fn take_resources(
        &mut self,
        author: [u8; 32],
        (app, doc): (&str, &str),
        envelope: &collab_types::SignedOp,
        named: Option<&str>,
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
        let mutation = match collab_types::DocumentFrame::decode(&payload)
            .map_err(|error| error.to_string())?
        {
            collab_types::DocumentFrame::Whole(mutation) => mutation,
            // A store document is far smaller than one frame holds.
            collab_types::DocumentFrame::Segment(_) => {
                return Err("a resource document's write came in segments".into());
            }
        };
        let (changes, named_in_document) = changes_of(mutation)?;
        let readers = known
            .or(named_in_document)
            .ok_or("a write of a resource document of readers this device never met")?;
        // Every change of one write is its author's: whether they may write the document is
        // asked once, of the workspace's one permission check.
        let permitted = self
            .may_start(author, (app, doc), tilo_clock::wall())
            .is_ok();
        let admit = move |_: &tilo_resource_store::Proposal<'_>| permitted;
        let received: Vec<Received> = changes
            .into_iter()
            .map(|change| Received { change, author })
            .collect();
        let taken = received.len();
        let refused = self.resources.receive(doc, &readers, received, &admit)?;
        let applied = refused.len() < taken;
        for (change, refusal) in refused {
            tracing::info!(workspace = %self.workspace(), %doc, %change, ?refusal, "a member's change to a resource was refused");
        }
        Ok(applied)
    }

    #[cfg(any(feature = "apps", feature = "agents"))]
    async fn draft_resource_document(
        &self,
        peer: &tilo_peer_state::StatePeer,
        device: &super::MemberDevice,
        app: &str,
        write: &tilo_resource_store::Written,
    ) -> Result<OwnerCommitRequest, String> {
        let now = tilo_clock::wall();
        let named = (collab_types::is_store_document(&write.doc)
            && write.readers != collab_types::EVERY_MEMBER)
            .then_some(write.readers.as_str());
        let isolated = peer.requires_document_keys(app, &write.doc, now)?;
        let group = if isolated {
            let workspace =
                collab_types::WorkspaceId::parse(self.workspace()).ok_or("invalid workspace id")?;
            neighbour_wire::KeyScope::document(workspace, app, &write.doc, named)
                .ok_or("the document has an invalid reader set")?
                .to_string()
        } else {
            self.workspace().to_owned()
        };
        // A write that founds its document opens the group when it has none yet, as a record's
        // first content does on every write path: the write lands sealed rather than failing.
        let founding = !peer.holds_document(app, &write.doc);
        let keys = self
            .device
            .as_ref()
            .ok_or("this device holds no group keys")?
            .group_keys(&group, &write.readers, isolated || founding)
            .await
            .ok_or("this device holds no keys of the document's readers yet")??;
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
        let (mut request, _) = tilo_peer_state::owner::draft_local_write(
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
                now: tilo_clock::wall(),
            },
        )
        .map_err(|error| error.to_string())?;
        if let CommitOperation::Document { envelope, .. } = &mut request.operation {
            collab_types::sign_op(envelope, &*device.signer).map_err(|error| error.to_string())?;
        }
        Ok(request)
    }
}

impl Workspaces {
    /// Restore the store projection of a sealed frame that passed peer admission.
    pub(super) async fn replay_resource_write(
        &self,
        governed: &mut GovernedWorkspace,
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
            || !governed
                .peer()
                .publication()
                .has_sealed_write(app_id, document_id, envelope)
        {
            return false;
        }
        #[cfg(any(feature = "apps", feature = "agents"))]
        governed.store_with_surfaces(&self.inner.host.surfaces);
        if let Err(error) = governed
            .take_resources(author, (app_id, document_id), envelope, readers.as_deref())
            .await
        {
            tracing::warn!(workspace = %governed.workspace(), %document_id, %error, "a retained resource write was not restored");
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
        let governed = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = governed.lock().await;
        let now = tilo_clock::wall();
        if governed
            .may_start(author, (app_id, document_id), now)
            .is_err()
        {
            return Ok(None);
        }
        let mut candidate = governed.peer().clone();
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
        if governed
            .take_resources(author, (app_id, document_id), envelope, readers.as_deref())
            .await?
        {
            // Whoever follows the workspace's writes (its bots) is woken by a resource's
            // change as by any, while the workspace is held, so it reads the change.
            self.tell_followers(&governed, author, &message, false)
                .await;
            self.wrote_frame(governed.workspace(), &message);
        }
        drop(governed);
        // The workspace's own replica keeps the document the store indexed, as it keeps every
        // document a member wrote: a change of it is drafted after it, and the workspace's other
        // members follow it as they follow any.
        Ok(Some(message))
    }

    /// Commit an existing, staged resource update while the workspace remains held.
    /// Check the same store and peer admissions that will take the retained frame.
    #[cfg(any(feature = "apps", feature = "agents"))]
    async fn commit_resource_update(
        &self,
        governed: &mut GovernedWorkspace,
        device: &super::MemberDevice,
        app: &str,
        write: &tilo_resource_store::Written,
        still_live: impl FnOnce(&GovernedWorkspace) -> bool,
    ) -> Result<(), String> {
        let document = &write.doc;
        let author = device.device();
        let now = tilo_clock::wall();
        governed.may_start(author, (app, document), now)?;
        // A staged edit is a delta. Recover its signed base from the same
        // retained history the workspace replays on open.
        if !governed.peer().holds_document(app, document) {
            self.recall(governed).await?;
            if !governed.peer().holds_document(app, document) {
                return Err("the workspace has not opened this resource's signed history".into());
            }
        }
        let mut candidate = governed.peer().clone();
        let request = self
            .admit_resource_write(governed, &mut candidate, device, app, write)
            .await?;
        if !still_live(governed) {
            return Err("the resource writer is no longer authorized".into());
        }
        self.keep_resource_write(governed, device, app, (write, &request), true)
            .await
    }

    #[cfg(any(feature = "apps", feature = "agents"))]
    async fn admit_resource_write(
        &self,
        governed: &GovernedWorkspace,
        candidate: &mut tilo_peer_state::StatePeer,
        device: &super::MemberDevice,
        app: &str,
        write: &tilo_resource_store::Written,
    ) -> Result<OwnerCommitRequest, String> {
        use neighbour_wire::NeighbourMsg;

        let document = &write.doc;
        let author = device.device();
        let now = tilo_clock::wall();
        governed.may_start(author, (app, document), now)?;
        let request = governed
            .draft_resource_document(candidate, device, app, write)
            .await?;
        let CommitOperation::Document {
            envelope, readers, ..
        } = &request.operation
        else {
            return Err("a resource write must name its document".into());
        };
        let message = NeighbourMsg::AppDocument {
            peer: neighbour_wire::peer_handle(&author),
            app_id: app.to_owned(),
            cluster_id: governed.workspace().to_owned(),
            document_id: document.clone(),
            envelope: envelope.clone(),
            readers: readers.clone(),
        };
        let device_keys = governed
            .device
            .as_ref()
            .ok_or("this device holds no group keys")?;
        let scoped = governed.peer().requires_document_keys(app, document, now)?;
        let keys = super::sealed_documents::read_document_keys_for(
            device_keys,
            governed.workspace(),
            (app, document),
            scoped,
            scoped,
            Some(write.readers.clone()),
        )
        .await?;
        candidate
            .receive_from(
                tilo_peer_state::TransportOrigin::RetainedServerDelta(author),
                message,
                now,
            )
            .map_err(|error| error.to_string())?;
        let opened = candidate
            .publication()
            .opened_document(app, document, &mut keys.open())?
            .ok_or("the staged resource document could not be opened")?;
        if !Automerge::load(&opened.document)
            .map_err(|error| error.to_string())?
            .has_change(&write.change.hash())
        {
            return Err("the staged resource change did not pass document admission".into());
        }
        Ok(request)
    }

    #[cfg(any(feature = "apps", feature = "agents"))]
    async fn keep_resource_write(
        &self,
        governed: &mut GovernedWorkspace,
        device: &super::MemberDevice,
        app: &str,
        (write, request): (&tilo_resource_store::Written, &OwnerCommitRequest),
        compact: bool,
    ) -> Result<(), String> {
        let author = device.device();
        let document = &write.doc;
        governed
            .store_with_surfaces(&self.inner.host.surfaces)
            .check_write(write, author)
            .map_err(|refusal| format!("the resource change was refused: {refusal:?}"))?;
        let (message, _) = self.retain_local_document(governed.workspace_id(), author, request)?;
        let neighbour_wire::NeighbourMsg::AppDocument {
            envelope, readers, ..
        } = &message
        else {
            return Err("a resource write must name its document".into());
        };
        if !governed
            .take_resources(author, (app, document), envelope, readers.as_deref())
            .await?
        {
            return Err("the retained resource change did not reach the store".into());
        }
        if governed.member.is_some() {
            self.follow_member_document(governed, author, &message);
            self.forward_member_document(governed.workspace_id(), message);
            self.wrote(governed.workspace(), app, document);
        } else {
            self.follow_write(
                governed,
                author,
                message.clone(),
                (app.to_owned(), document.clone()),
                compact,
            )
            .await;
        }
        Ok(())
    }

    /// Answer an app's read `command` from the store of the workspace `state` holds, with only
    /// the resources whose reader set `may_read` admits.
    #[cfg(any(feature = "apps", feature = "agents"))]
    async fn resource_read(
        &self,
        state: &tokio::sync::Mutex<GovernedWorkspace>,
        may_read: &(dyn Fn(&str) -> bool + Sync),
        command: &tilo_resource_store::wire::Command,
    ) -> Result<tilo_resource_store::wire::Reply, String> {
        use tilo_resource_store::wire::{Command, Reply, read};
        let mut governed = state.lock().await;
        let store = governed.store_with_surfaces(&self.inner.host.surfaces);
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
            _ => read(store, command, may_read).unwrap_or(Reply::Written),
        };
        drop(governed);
        Ok(answer)
    }

    /// Commit `command` as an update of the fields of a resource every member reads that stays
    /// where it is, when it is one: `false` for any other command, which is drafted whole.
    #[cfg(any(feature = "apps", feature = "agents"))]
    async fn put_in_place(
        &self,
        governed: &mut GovernedWorkspace,
        device: &super::MemberDevice,
        app: &str,
        command: &tilo_resource_store::wire::Command,
    ) -> Result<bool, String> {
        let tilo_resource_store::wire::Command::Put {
            kind,
            id,
            resource,
            stamp,
        } = command
        else {
            return Ok(false);
        };
        let in_place = governed.member.is_none()
            && resource.readers == collab_types::EVERY_MEMBER
            && governed
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
        let admit = |_: &tilo_resource_store::Proposal<'_>| true;
        let write = governed
            .store_with_surfaces(&self.inner.host.surfaces)
            .draft_field_update((kind, id), resource, stamp, device.device(), None, &admit)?;
        if let Some(write) = write {
            self.commit_resource_update(governed, device, app, &write, |_| true)
                .await?;
        }
        Ok(true)
    }

    /// Carries out an app's command on its certified route. A write requires the caller's
    /// matching local signer, and a read returns only what this device's person may read.
    /// A batch the store refused partway commits what it wrote before the refusal.
    ///
    /// # Errors
    /// The workspace is not held here, the store refused the write, or the workspace refused a
    /// document's write.
    #[cfg(feature = "apps")]
    pub async fn resource_command(
        &self,
        context: &crate::apps::services::ResourceRequestContext,
        mut command: tilo_resource_store::wire::Command,
    ) -> Result<tilo_resource_store::wire::Reply, String> {
        let workspace = context.workspace_id();
        if let tilo_resource_store::wire::Command::Turn {
            address,
            inputs,
            answer,
        } = command
        {
            return self
                .once_turn(workspace, context.app_id(), address, inputs, answer)
                .await
                .map(tilo_resource_store::wire::Reply::Turn);
        }
        let device = self.member_identity()?;
        let writing = command.writes();
        if writing {
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
            let person = tilo_actions::Actor::Member(tilo_actions::MemberId::person(person));
            stamp_resource_invoker(&mut command, &|invocation| {
                context
                    .invoker_of(invocation)
                    .cloned()
                    .unwrap_or_else(|| person.clone())
            });
        }
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        if !writing {
            let may_read = may_read(hex::encode(device.cert.user_public_key));
            return self.resource_read(&state, &may_read, &command).await;
        }
        let governed = state.lock_owned().await;
        self.write_resources(governed, device, context.app_id(), command)
            .await
    }

    /// Whether this device runs `app`'s action `address` on `inputs`, which runs once in
    /// `workspace` ([`crate::work::Turn::Once`]); with `done`, keep that it ran and let the turn
    /// go. Asking again while this device runs it renews its claim.
    #[cfg(feature = "apps")]
    async fn once_turn(
        &self,
        workspace: &str,
        app: &str,
        address: String,
        inputs: serde_json::Value,
        answer: Option<serde_json::Value>,
    ) -> Result<tilo_resource_store::wire::Turn, String> {
        use crate::work::{NotTaken, Turn};
        use tilo_resource_store::wire::Turn as Answer;
        if address.split_once('/').map(|(of, _)| of) != Some(app) {
            return Err(format!(
                "{app} runs only its own actions once, not {address}"
            ));
        }
        let turn = Turn::Once { address, inputs };
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

    /// Carry out the write `command` of `app` on the workspace `governed` holds, as `device`,
    /// holding the workspace until the last of its frames is kept.
    #[cfg(any(feature = "apps", feature = "agents"))]
    async fn write_resources(
        &self,
        mut governed: tokio::sync::OwnedMutexGuard<GovernedWorkspace>,
        device: std::sync::Arc<super::MemberDevice>,
        app: &str,
        command: tilo_resource_store::wire::Command,
    ) -> Result<tilo_resource_store::wire::Reply, String> {
        use tilo_resource_store::wire::Reply;
        if self
            .put_in_place(&mut governed, &device, app, &command)
            .await?
        {
            drop(governed);
            return Ok(Reply::Written);
        }

        let (writes, refusal) = match tilo_resource_store::wire::draft(
            governed.store_with_surfaces(&self.inner.host.surfaces),
            command,
        ) {
            Ok(writes) => (writes, None),
            Err(refused) => (refused.written, Some(refused.error.to_string())),
        };
        let app = app.to_owned();
        let mut candidate = governed.peer().clone();
        let mut documents = BTreeSet::new();
        if writes.iter().any(|write| {
            documents.insert(&write.doc)
                && !write.change.deps().is_empty()
                && !candidate.holds_document(&app, &write.doc)
        }) {
            if governed.member.is_some() {
                self.recall_member_resources(&mut governed, None).await?;
            } else {
                self.recall(&mut governed).await?;
            }
            candidate = governed.peer().clone();
        }
        let mut requests = Vec::with_capacity(writes.len());
        for write in &writes {
            requests.push(
                self.admit_resource_write(&governed, &mut candidate, &device, &app, write)
                    .await?,
            );
        }

        if governed.member.is_some() && !requests.is_empty() {
            self.wait_member_link(governed.workspace_id()).await?;
        }
        let (complete, answer) = tokio::sync::oneshot::channel();
        let workspaces = self.clone();
        // Once retention starts, finish applying that frame even if the caller goes away.
        // A cancelled caller never starts the next frame of a batch.
        tokio::spawn(async move {
            let result = async {
                for (index, (write, request)) in writes.iter().zip(&requests).enumerate() {
                    if complete.is_closed() {
                        return Err("the resource writer was cancelled".into());
                    }
                    workspaces
                        .keep_resource_write(
                            &mut governed,
                            &device,
                            &app,
                            (write, request),
                            // Compaction can consume a sequence the batch already drafted.
                            index.saturating_add(1) == requests.len(),
                        )
                        .await?;
                }
                refusal.map_or(Ok(Reply::Written), Err)
            }
            .await;
            let _ = complete.send(result);
        });
        answer
            .await
            .map_err(|_| "the resource writer stopped".to_owned())?
    }
}

impl Workspaces {
    /// [`Self::resource_command`] as this device's own person, for an app the device starts
    /// itself rather than through the broker: the caller is the device the service signs as.
    ///
    /// # Errors
    /// As [`Self::resource_command`], or this service serves no device.
    #[cfg(feature = "apps")]
    pub async fn own_resource_command(
        &self,
        workspace: &str,
        app: &str,
        command: tilo_resource_store::wire::Command,
    ) -> Result<tilo_resource_store::wire::Reply, String> {
        let device = self.member_identity()?;
        let caller = tilo_actions::remote::Caller::authenticated(
            device.cert.user_public_key,
            device.device(),
        )
        .in_workspace(workspace);
        let context =
            crate::apps::services::ResourceRequestContext::authenticated(app, workspace, caller);
        self.resource_command(&context, command).await
    }

    /// Carry out `command` of `app`'s record of `workspace` for one of this device's own roles
    /// (the agent engine, a bot, a bridge), with no window and no broker between them.
    ///
    /// A read answers everything the device holds, as the role's own runtime then decides what
    /// each member it reads for may see. A write is signed by the device the service signs its
    /// own roles' writes as, and keeps the invoker the role's runtime stamped on it: the member
    /// whose action wrote it (an agent, a bot, a person whose event ran). A bot's write is
    /// decided here under the grants the workspace holds for that bot, as every member decides
    /// a bot's change to a record it signs itself. Only code in this process
    /// reaches this; a caller from outside comes through [`Self::resource_command`], which
    /// stamps who it is.
    ///
    /// # Errors
    /// [`RecordFailure::Refused`] when the workspace refused a bot's write;
    /// [`RecordFailure::Failed`] when the workspace is not held here, the store refused the
    /// write, or the workspace refused a document's write.
    #[cfg(feature = "agents")]
    pub async fn record_command(
        &self,
        workspace: &str,
        app: &str,
        command: tilo_resource_store::wire::Command,
    ) -> Result<tilo_resource_store::wire::Reply, RecordFailure> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        if !command.writes() {
            return Ok(self.resource_read(&state, &|_| true, &command).await?);
        }
        let mut governed = state.lock_owned().await;
        bot_writes::decide(
            &mut governed,
            (
                &self.inner.host.surfaces,
                self.inner.host.cert.device_public_key,
            ),
            app,
            &command,
        )
        .map_err(RecordFailure::Refused)?;
        Ok(self
            .write_resources(governed, self.signing_device(), app, command)
            .await?)
    }
}

impl Workspaces {
    /// Every document `workspace`'s resource store keeps, whole, history and all: what a check
    /// that something was never kept anywhere in the workspace reads.
    ///
    /// # Errors
    /// The workspace is not held here.
    #[cfg(feature = "test-support")]
    pub async fn store_documents(&self, workspace: &str) -> Result<Vec<Vec<u8>>, String> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        let documents: Vec<String> = governed
            .resources
            .store_mut()
            .index()
            .documents()
            .map(str::to_owned)
            .collect();
        let stored = documents
            .iter()
            .filter_map(|doc| governed.resources.document(doc))
            .collect();
        drop(governed);
        Ok(stored)
    }

    /// The sealed writes of each store document of `app` that `workspace` holds, oldest first,
    /// by document: what a member is sent of the resources a tilolang app's actions wrote.
    ///
    /// # Errors
    /// The workspace is not held here.
    #[cfg(feature = "test-support")]
    pub async fn store_writes(
        &self,
        workspace: &str,
        app: &str,
    ) -> Result<BTreeMap<String, Vec<collab_types::SignedOp>>, String> {
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        // The peer guard borrows `governed`, so each document is named before it is taken.
        #[allow(
            clippy::needless_collect,
            reason = "the peer guard borrows the store the names are read from, so the names are taken first"
        )]
        let documents: Vec<String> = governed
            .resources
            .store_mut()
            .index()
            .documents()
            .map(str::to_owned)
            .collect();
        let mut sealed = BTreeMap::new();
        for document in documents {
            let writes = governed.peer().publication().sealed_history(app, &document);
            if !writes.is_empty() {
                sealed.insert(document, writes);
            }
        }
        drop(governed);
        Ok(sealed)
    }

    /// Holds `query` open for the app `context` names, in its workspace: its first page, then
    /// each page a write here or a member's change makes different, as this device's person
    /// may read it.
    ///
    /// # Errors
    /// The workspace is not held here, or this service serves no device.
    #[cfg(feature = "apps")]
    pub(crate) async fn open_resources(
        &self,
        context: &crate::apps::services::ResourceRequestContext,
        query: tilo_resource_store::Query,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<tilo_resource_store::Page>, String> {
        let reader = hex::encode(self.member_identity()?.cert.user_public_key);
        let state = self
            .workspace(context.workspace_id())
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = state.lock().await;
        governed.store_with_surfaces(&self.inner.host.surfaces);
        Ok(governed.resources.open_query(query, reader))
    }
}

/// The command names the verb and invocation, and the workspace service names who made it: whoever invoked
/// the invocation a write serves, when the workspace service delivered that one to the app, and the person
/// the route certifies otherwise.
#[cfg(feature = "apps")]
fn stamp_resource_invoker(
    command: &mut tilo_resource_store::wire::Command,
    invoker: &dyn Fn(&str) -> tilo_actions::Actor,
) {
    use tilo_resource_store::wire::Command;
    match command {
        Command::Put { stamp, .. }
        | Command::Remove { stamp, .. }
        | Command::Undo { stamp, .. }
        | Command::Edit { stamp, .. } => {
            stamp.invoker = tilo_resource_store::Stamp::by(&invoker(&stamp.invocation));
        }
        Command::Batch(commands) => commands
            .iter_mut()
            .for_each(|command| stamp_resource_invoker(command, invoker)),
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
        | Command::Position { .. }
        | Command::PageAt { .. }
        | Command::Contents { .. }
        | Command::Fields { .. }
        | Command::Turn { .. } => {}
    }
}

/// Whether the reader set `readers` names `reader`: `*` names everyone.
#[cfg(feature = "apps")]
fn may_read(reader: String) -> impl Fn(&str) -> bool {
    move |readers| readers == "*" || readers.split(',').any(|person| person == reader)
}

// The tests write and read a device's resources through the store (`apps`) and
// the members around it (`agents`, for the device the sealed fixtures make).
#[cfg(all(test, feature = "apps", feature = "agents"))]
mod tests;
