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
        for entry in fs::read_dir(&docs).into_iter().flatten().flatten() {
            let Some(doc) = doc_of(&entry.file_name()) else {
                continue;
            };
            let restored = read_snapshot(&root, &doc)
                .is_some_and(|(heads, snapshot)| store.restore(&doc, &snapshot, &heads));
            if restored {
                continue;
            }
            let loaded = fs::read(entry.path())
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
        }
    }

    /// The store, for whoever reads the workspace's resources.
    #[cfg(all(test, feature = "agents"))]
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

    /// Keeps what `doc` took since `before`: its changes appended to its file, its snapshot
    /// dropped first, as it no longer says where the document is.
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
        self.unsnapshotted.insert(doc.to_owned());
        self.used.insert(doc.to_owned(), tilo_clock::now());
        self.evict_idle();
        Ok(())
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
        let index = self.root.join(INDEX);
        fs::create_dir_all(&docs)?;
        fs::create_dir_all(&index)?;
        write_whole(&docs.join(file_name(doc)), bytes)?;
        if let Some(snapshot) = self.store_mut().snapshot(doc) {
            let heads: Vec<[u8; 32]> = self.heads(doc).iter().map(|head| head.0).collect();
            let kept = postcard::to_stdvec(&(heads, snapshot))
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            write_whole(&index.join(file_name(doc)), &kept)?;
        }
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

/// The snapshot kept of `doc` and the heads it was taken at.
fn read_snapshot(root: &Path, doc: &str) -> Option<(Vec<ChangeHash>, Vec<u8>)> {
    let bytes = fs::read(root.join(INDEX).join(file_name(doc))).ok()?;
    let (heads, snapshot): (Vec<[u8; 32]>, Vec<u8>) = postcard::from_bytes(&bytes).ok()?;
    Some((heads.into_iter().map(ChangeHash).collect(), snapshot))
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
}

impl Workspaces {
    /// Takes `message`, `author`'s write, into `workspace`'s store when it writes a document the
    /// store placed; hands any other write back.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays held until its followers are told, so they read the write"
    )]
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
        Ok(None)
    }
}

#[cfg(all(test, feature = "agents"))]
mod tests;
