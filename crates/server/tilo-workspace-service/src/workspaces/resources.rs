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
#[cfg(feature = "apps")]
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
#[cfg(feature = "apps")]
use tilo_resource_store::Schema;
use tilo_resource_store::{Admit, Received, Refusal, Store};

use super::{GovernedWorkspace, Workspaces};

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
    /// How many surfaces this process had named when the store last declared them
    /// ([`collab_types::app_surface::registered`]).
    #[cfg(feature = "apps")]
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
            #[cfg(feature = "apps")]
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

    /// The workspace's store, with the types and verbs of every app surface this process knows
    /// declared: what this device's writes and the members' are judged by.
    #[cfg(feature = "apps")]
    fn store_with_surfaces(&mut self) -> &mut Store {
        let named = collab_types::app_surface::registered();
        if self.resources.declared != named {
            self.resources.declared = named;
            for file in collab_types::app_surface::every() {
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
                let Some(Ok(keys)) = self.reader_keys(readers).await else {
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

    /// Makes the write `command` in the store as `device`, and answers each document's change
    /// prepared to commit as this device's signed write: sealed for the document's reader set,
    /// the whole document where the replica holds none of it yet. A batch the store refused
    /// partway answers what it wrote before the refusal, which the store holds, with the refusal.
    ///
    /// # Errors
    /// This device holds no keys of a document's readers, or a document's write could not be
    /// drafted.
    #[cfg(feature = "apps")]
    pub(super) async fn prepare_resource_write(
        &mut self,
        device: &super::MemberDevice,
        app: &str,
        command: tilo_resource_store::wire::Command,
    ) -> Result<(Vec<OwnerCommitRequest>, Option<String>), String> {
        let (written, refused) =
            match tilo_resource_store::wire::write(self.store_with_surfaces(), command) {
                Ok(written) => (written, None),
                Err(refused) => {
                    let refusal = refused.error.to_string();
                    (refused.written, Some(refusal))
                }
            };
        // What the store took is kept before it answers: a document nobody's later change
        // reaches again would otherwise leave memory with nothing of it on disk.
        for write in &written {
            self.resources.kept(&write.doc, write.change.deps())?;
        }
        let now = tilo_clock::wall();
        let mut requests = Vec::with_capacity(written.len());
        for write in written {
            let readers = write.readers.clone();
            let keys = match self.reader_keys(&readers).await {
                Some(Ok(keys)) => keys,
                Some(Err(error)) => return Err(error),
                None => {
                    return Err(
                        "this device holds no keys of the document's readers yet".to_owned()
                    );
                }
            };
            // The store placed the document; the replica holds it only once a write was followed
            // there, and a change needs the document it changes.
            let founding = !self.peer().holds_document(app, &write.doc);
            let bytes = if founding {
                self.store_with_surfaces()
                    .document(&write.doc)
                    .map(Automerge::save)
                    .ok_or_else(|| format!("the store holds no document of {}", write.doc))?
            } else {
                write.change.raw_bytes().to_vec()
            };
            let frame = if founding {
                collab_types::DocumentMutation::Snapshot(bytes)
            } else {
                collab_types::DocumentMutation::Incremental(bytes)
            }
            .encode()
            .map_err(|error| error.to_string())?;
            let artifact = collab_types::document_artifact_id(app, self.workspace(), &write.doc);
            let mut seal = |plaintext: &[u8]| {
                keys.seal_content(
                    artifact,
                    collab_types::ContentRetention::of_frame(plaintext),
                    plaintext,
                )
                .map_err(|error| error.to_string())
            };
            let mut envelope = self.peer().draft_write(
                (app, &write.doc),
                &frame,
                Some(&mut seal),
                device.device(),
                0,
                now,
            )?;
            collab_types::sign_op(&mut envelope, &*device.signer)
                .map_err(|error| error.to_string())?;
            let own = self.peer().own_write(app, &write.doc, &device.device());
            let named = (collab_types::is_store_document(&write.doc)
                && readers != collab_types::EVERY_MEMBER)
                .then_some(readers);
            requests.push(OwnerCommitRequest {
                workspace: self.workspace().to_owned(),
                operation: CommitOperation::Document {
                    app: app.to_owned(),
                    document: write.doc,
                    envelope: Box::new(envelope),
                    own,
                    readers: named,
                },
            });
        }
        Ok((requests, refused))
    }
}

impl Workspaces {
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
        let governed = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let mut governed = governed.lock().await;
        if governed
            .take_resources(author, (app_id, document_id), envelope, readers.as_deref())
            .await?
        {
            // Whoever follows the workspace's writes (its bots) is woken by a resource's
            // change as by any, while the workspace is held, so it reads the change.
            let written = (app_id.clone(), document_id.clone());
            self.tell_followers(&governed, written, &message).await;
        }
        drop(governed);
        // A write by a device of no editor changes nothing here, and is no document write
        // either: it was dropped as any write the workspace refuses is.
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let permitted = {
            let governed = state.lock().await;
            governed
                .may_start(author, (app_id, document_id), tilo_clock::wall())
                .is_ok()
        };
        // The workspace's own replica keeps the document the store indexed, as it keeps every
        // document a member wrote: a change of it is drafted after it, and the workspace's other
        // members follow it as they follow any.
        Ok(permitted.then_some(message))
    }

    /// Carries out an app's command on its certified route. A write requires the caller's
    /// matching local signer, and a read returns only what this device's person may read.
    /// A batch the store refused partway commits what it wrote before the refusal.
    ///
    /// # Errors
    /// The workspace is not held here, the store refused the write, or the workspace refused a
    /// document's write.
    #[cfg(feature = "apps")]
    pub(crate) async fn resource_command(
        &self,
        context: &crate::apps::services::ResourceRequestContext,
        mut command: tilo_resource_store::wire::Command,
    ) -> Result<tilo_resource_store::wire::Reply, String> {
        use tilo_resource_store::wire::{Command, Reply, read};
        let workspace = context.workspace_id();
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
            stamp_resource_invoker(&mut command, person);
        }
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        if !writing {
            let may_read = may_read(hex::encode(device.cert.user_public_key));
            let mut governed = state.lock().await;
            let store = governed.store_with_surfaces();
            let answer = match &command {
                Command::Versions { kind, id } if store.get(kind, id, &may_read).is_some() => {
                    Reply::Versions(
                        store
                            .read_versions(kind, id)
                            .map_err(|error| error.to_string())?,
                    )
                }
                Command::Versions { .. } => Reply::Versions(Vec::new()),
                _ => read(store, &command, &may_read).unwrap_or(Reply::Written),
            };
            drop(governed);
            return Ok(answer);
        }
        let (requests, refused) = {
            let mut governed = state.lock().await;
            governed
                .prepare_resource_write(&device, context.app_id(), command)
                .await?
        };
        for request in requests {
            self.commit_local_document(workspace, request).await?;
        }
        refused.map_or(Ok(Reply::Written), Err)
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
        governed.store_with_surfaces();
        Ok(governed.resources.open_query(query, reader))
    }
}

/// The command names the verb and invocation, and the broker names who made it.
#[cfg(feature = "apps")]
fn stamp_resource_invoker(command: &mut tilo_resource_store::wire::Command, invoker: &str) {
    use tilo_resource_store::wire::Command;
    match command {
        Command::Put { stamp, .. }
        | Command::Remove { stamp, .. }
        | Command::Undo { stamp, .. }
        | Command::Edit { stamp, .. } => invoker.clone_into(&mut stamp.invoker),
        Command::Batch(commands) => commands
            .iter_mut()
            .for_each(|command| stamp_resource_invoker(command, invoker)),
        Command::Get { .. }
        | Command::Query(_)
        | Command::Count { .. }
        | Command::Content { .. }
        | Command::Versions { .. } => {}
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
