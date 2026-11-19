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
use tilo_resource_store::{Admit, Received, Refusal, Store};
#[cfg(feature = "apps")]
use tilo_resource_store::{Indexed, Schema, StoreError, Written};

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
            #[cfg(feature = "apps")]
            declared: 0,
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

    /// Writes `write` through the store as `device`, and answers each document's change prepared
    /// to commit as this device's signed write: sealed for the document's reader set, the whole
    /// document where the replica holds none of it yet.
    ///
    /// # Errors
    /// The store refused the write, this device holds no keys of a document's readers, or a
    /// document's write could not be drafted.
    #[cfg(feature = "apps")]
    pub(super) async fn prepare_resource_write(
        &mut self,
        device: &super::MemberDevice,
        app: &str,
        write: impl FnOnce(&mut Store) -> Result<Vec<Written>, StoreError>,
    ) -> Result<Vec<OwnerCommitRequest>, String> {
        let written = {
            let store = self.store_with_surfaces();
            write(store).map_err(|error| error.to_string())?
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
        Ok(requests)
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

    /// The person key the store's reader sets name for this device's person.
    #[cfg(feature = "apps")]
    fn resource_reader(&self) -> Result<String, String> {
        Ok(hex::encode(self.member_identity()?.cert.user_public_key))
    }

    /// `app`'s document `document` of `workspace`, written through its store as this device, and
    /// committed as this device's signed write in every document it changed.
    ///
    /// # Errors
    /// The workspace is not held here, the store refused the write, or the workspace refused a
    /// document's write.
    #[cfg(feature = "apps")]
    pub(crate) async fn write_resources(
        &self,
        workspace: &str,
        app: &str,
        write: impl FnOnce(&mut Store) -> Result<Vec<Written>, StoreError>,
    ) -> Result<(), String> {
        let device = self.member_identity()?;
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let requests = {
            let mut governed = state.lock().await;
            governed.prepare_resource_write(&device, app, write).await?
        };
        for request in requests {
            self.commit_local_document(workspace, request).await?;
        }
        Ok(())
    }

    /// The resource `kind`'s `id` of `workspace`, as this device's person may read it.
    ///
    /// # Errors
    /// The workspace is not held here.
    #[cfg(feature = "apps")]
    pub(crate) async fn resource_get(
        &self,
        workspace: &str,
        kind: &str,
        id: &str,
    ) -> Result<Option<Indexed>, String> {
        let may_read = may_read(self.resource_reader()?);
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let answer = {
            let mut governed = state.lock().await;
            governed
                .store_with_surfaces()
                .get(kind, id, &may_read)
                .cloned()
        };
        Ok(answer)
    }

    /// One page of `query` in `workspace`, as this device's person may read it.
    ///
    /// # Errors
    /// The workspace is not held here.
    #[cfg(feature = "apps")]
    pub(crate) async fn resource_query(
        &self,
        workspace: &str,
        query: &tilo_resource_store::Query,
    ) -> Result<tilo_resource_store::Page, String> {
        let may_read = may_read(self.resource_reader()?);
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let answer = {
            let mut governed = state.lock().await;
            governed.store_with_surfaces().query(query, &may_read)
        };
        Ok(answer)
    }

    /// How many resources of `kind` hold to `filters` in `workspace`, as this device's person may
    /// read them.
    ///
    /// # Errors
    /// The workspace is not held here.
    #[cfg(feature = "apps")]
    pub(crate) async fn resource_count(
        &self,
        workspace: &str,
        kind: &str,
        filters: &[tilo_resource_store::Filter],
    ) -> Result<usize, String> {
        let may_read = may_read(self.resource_reader()?);
        let state = self
            .workspace(workspace)
            .await
            .map_err(|error| error.to_string())?;
        let answer = {
            let mut governed = state.lock().await;
            governed
                .store_with_surfaces()
                .count(kind, filters, &may_read)
        };
        Ok(answer)
    }
}

/// Whether the reader set `readers` names `reader`: `*` names everyone.
#[cfg(feature = "apps")]
fn may_read(reader: String) -> impl Fn(&str) -> bool {
    move |readers| readers == "*" || readers.split(',').any(|person| person == reader)
}

#[cfg(all(test, feature = "agents"))]
mod tests;
