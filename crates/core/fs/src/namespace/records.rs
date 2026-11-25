//! The namespace as the records the tree is kept in: what a device holding the tree reads
//! from them, takes in from them and writes to them, and what it keeps of itself on disk.

use std::collections::{BTreeMap, HashSet};

use automerge::{AutoCommit, ObjId};
use fs_protocol::TreeChunk;
use tilo_automerge::ApplyIncoming as _;

use super::changes::ChangeTrackedDoc;
use super::chunks::Chunks;
use super::{Namespace, ROOT_DOCUMENT_ID_KEY, ROOT_ID, ROOT_INO, str_at_doc};

impl Namespace {
    /// A namespace as [`Self::save`] kept it.
    ///
    /// # Errors
    /// `data` is not what [`Self::save`] writes, or a record in it is no document.
    pub fn from_bytes(
        author: impl Into<super::MountAuthor>,
        data: &[u8],
    ) -> Result<Self, automerge::AutomergeError> {
        Self::from_records(author, unbundle(data)?)
    }

    /// A namespace holding the tree's documents, each the number of its chunk (0 for the
    /// tree's own) and the document saved whole: what a device that holds the tree's documents
    /// starts from.
    ///
    /// # Errors
    /// One of them does not load.
    pub fn from_records(
        author: impl Into<super::MountAuthor>,
        documents: impl IntoIterator<Item = (TreeChunk, Vec<u8>)>,
    ) -> Result<Self, automerge::AutomergeError> {
        Self::from_documents_read(author, documents).map(|(namespace, _)| namespace)
    }

    /// [`Self::from_records`], with the heads each document stood at as read: opening may
    /// change the documents (naming what it wrote), and what keeps them on disk keeps those
    /// changes too ([`crate::kept_documents`]).
    ///
    /// # Errors
    /// One of them does not load.
    pub fn from_documents_read(
        author: impl Into<super::MountAuthor>,
        documents: impl IntoIterator<Item = (TreeChunk, Vec<u8>)>,
    ) -> Result<(Self, BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>), automerge::AutomergeError>
    {
        let mut index = None;
        let mut chunks = Vec::new();
        for (chunk, bytes) in documents {
            let doc = AutoCommit::load(&bytes)?;
            if chunk == 0 {
                index = Some(doc);
            } else {
                chunks.push((chunk, doc));
            }
        }
        let mount: super::MountAuthor = author.into();
        let mut docs = Chunks::new(
            index.unwrap_or_default(),
            mount.workspace.clone(),
            mount.person.clone(),
        );
        for (chunk, doc) in chunks {
            docs.adopt_chunk(chunk, doc)?;
        }
        docs.locate_all();
        let read: BTreeMap<TreeChunk, Vec<automerge::ChangeHash>> = docs
            .each_mut()
            .map(|(chunk, doc)| (chunk, doc.get_heads()))
            .collect();
        // Resume the id counter past ids we authored before, so a restart can't reuse a node id.
        let prefix = format!("{}/", mount.author);
        let max_local = docs
            .node_ids()
            .filter_map(|id| id.strip_prefix(&prefix)?.parse::<u64>().ok())
            .map(|n| n.saturating_add(1))
            .max()
            .unwrap_or(0);
        let mut ns = Self::over(docs, mount, max_local);
        ns.rebuild_document_index();
        // Name what opening wrote as the mount's now: a merge made before the next save settles
        // a pending change without its name, and a device signs only the changes that name what
        // made them.
        for (_, doc) in ns.doc.each_mut() {
            doc.commit_named();
        }
        Ok((ns, read))
    }

    /// Every record of the tree, kept as one: what [`Self::from_bytes`] reads back.
    pub fn save(&mut self) -> Vec<u8> {
        bundle(
            self.doc
                .each_mut()
                .map(|(chunk, doc)| (chunk, doc.save()))
                .collect(),
        )
    }

    /// Record `chunk` of the tree (0 for the tree's own) saved whole, if this namespace holds it.
    pub fn save_document(&mut self, chunk: TreeChunk) -> Option<Vec<u8>> {
        Some(self.doc.doc_mut(chunk)?.save())
    }

    /// The records of the tree this namespace holds: the tree's own (0) and each chunk.
    #[must_use]
    pub fn records(&self) -> Vec<TreeChunk> {
        self.doc.records().collect()
    }

    /// Every record of the tree this namespace holds or the tree's own record names, the tree's
    /// own first: what a device publishing the tree keeps a channel for. A chunk the record lists
    /// but this namespace holds no nodes of is one a holder of the tree gives it whole.
    #[must_use]
    pub fn listed_records(&self) -> Vec<TreeChunk> {
        let mut records = self.records();
        for chunk in fs_protocol::tree::listed_chunks(&**self.doc.index()) {
            if !records.contains(&chunk) {
                records.push(chunk);
            }
        }
        records.sort_unstable();
        records
    }

    /// Changes since the last `save_incremental`/load/merge, advancing the shared
    /// automerge watermark.
    ///
    /// INVARIANT: pair every mutation with its `save_incremental` inside the same
    /// `Namespace` lock scope: the watermark is one shared cursor, so a mutation saved
    /// outside that scope can have its delta swallowed by a concurrent remote apply.
    pub fn save_incremental(&mut self) -> Vec<u8> {
        bundle(
            self.doc
                .each_mut()
                .map(|(chunk, doc)| (chunk, doc.save_incremental()))
                .collect(),
        )
    }

    pub fn apply_incremental(&mut self, data: &[u8]) -> Result<(), automerge::AutomergeError> {
        let touched = self.load_records(unbundle(data)?)?;
        self.index_after_apply(&touched);
        Ok(())
    }

    pub fn prime_baseline(&mut self) {
        for (_, doc) in self.doc.each_mut() {
            let _ = doc.save_incremental();
        }
    }

    pub fn merge_doc(&mut self, other: &mut Namespace) -> Result<(), automerge::AutomergeError> {
        let records: Vec<(TreeChunk, Vec<u8>)> = other
            .doc
            .each_mut()
            .map(|(chunk, doc)| (chunk, doc.save()))
            .collect();
        let touched = self.load_records(records)?;
        self.index_after_apply(&touched);
        Ok(())
    }

    /// Apply a remote delta: advance the watermark, resolve, and return the resolution
    /// delta to broadcast (empty if none).
    pub fn apply_remote_and_resolve(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, automerge::AutomergeError> {
        let touched = self.load_records(unbundle(data)?)?;
        for (_, doc) in self.doc.each_mut() {
            let _ = doc.save_incremental();
        }
        self.index_after_apply(&touched);
        self.resolve_pending();
        Ok(self.save_incremental())
    }

    pub fn merge_remote_and_resolve(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, automerge::AutomergeError> {
        self.apply_remote_and_resolve(data)
    }

    /// Take in the tree's records as [`Self::save`] keeps them, as a mount joins the tree its
    /// workspace keeps ([`Self::take_documents`]), and return what resolving them changed.
    pub fn adopt_or_merge_remote_and_resolve(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, automerge::AutomergeError> {
        let records = unbundle(data)?;
        for (_, doc) in self.doc.each_mut() {
            let _ = doc.save_incremental();
        }
        self.take_documents(records)?;
        Ok(self.save_incremental())
    }

    /// Take in what the tree's records hold, each the number of its chunk (0 for the tree's own
    /// record) and a document saved whole or the changes of one: a device holding the tree
    /// merges what its workspace's records hold. The changes resolving what they brought
    /// (a cycle two moves made) are this namespace's own, and go out with its next
    /// [`Self::document_writes`].
    ///
    /// A namespace that holds no node yet takes the tree's own record as it is, under the
    /// tree's root identity, rather than merging a second root into it.
    ///
    /// # Errors
    /// A record does not load; what loaded before it is kept.
    pub fn take_documents(
        &mut self,
        records: Vec<(TreeChunk, Vec<u8>)>,
    ) -> Result<(), automerge::AutomergeError> {
        self.take_documents_touching(records).map(drop)
    }

    /// [`Self::take_documents`], answering the nodes the records touched: what a follower of the
    /// tree reads again, rather than every node. A namespace that adopted the tree's own record
    /// answers every node.
    ///
    /// # Errors
    /// A record does not load; what loaded before it is kept.
    pub fn take_documents_touching(
        &mut self,
        records: Vec<(TreeChunk, Vec<u8>)>,
    ) -> Result<HashSet<String>, automerge::AutomergeError> {
        let mut records = records;
        if !self.holds_tree()
            && let Some(at) = records.iter().position(|(chunk, _)| *chunk == 0)
        {
            let mut adopted = AutoCommit::new();
            adopted.load_incoming(&records.get(at).map(|(_, b)| b.clone()).unwrap_or_default())?;
            // A record whose own document carries no root yet adopts nothing: this namespace's
            // root founds it.
            if str_at_doc(&adopted, ROOT_DOCUMENT_ID_KEY).is_some() {
                records.swap_remove(at);
                let listed = fs_protocol::tree::listed_chunks(&**self.doc.index());
                self.doc.adopt_chunk(0, adopted)?;
                for chunk in listed {
                    fs_protocol::tree::list_chunk(self.doc.index_mut(), chunk)?;
                }
                self.ino_to_id.retain(|ino, _| *ino == ROOT_INO);
                self.id_to_ino.retain(|id, _| id == ROOT_ID);
                self.cycle_scan_pending = true;
                self.placed_since_resolve.clear();
                let touched = self.load_records(records)?;
                drop(touched);
                self.rebuild_document_index();
                self.resolve_pending();
                return Ok(self.doc.node_ids().cloned().collect());
            }
        }
        let touched = self.load_records(records)?;
        self.index_after_apply(&touched);
        self.resolve_pending();
        Ok(touched)
    }

    /// The chunks the tree's own record lists: every chunk of the tree, held here or not.
    #[must_use]
    pub fn listed_chunks(&self) -> Vec<TreeChunk> {
        fs_protocol::tree::listed_chunks(&**self.doc.index())
    }

    /// Each record's heads: the tree's own (0) and every chunk's.
    pub fn document_heads(&mut self) -> BTreeMap<TreeChunk, Vec<automerge::ChangeHash>> {
        self.doc.heads()
    }

    /// Whether record `chunk` holds the change `hash`: what a change delivered before the record it
    /// builds on finds missing.
    pub fn holds_change(&mut self, chunk: TreeChunk, hash: &automerge::ChangeHash) -> bool {
        self.doc
            .doc_mut(chunk)
            .is_some_and(|doc| doc.has_change(hash))
    }

    /// Take in what another device published of the tree ([`Self::take_documents`]), and answer
    /// the heads the tree's holders have of each of its documents now: what they had
    /// (`published`, as this device published it) and the changes that arrived.
    ///
    /// This device's own changes are not among them: neither the ones it wrote since it last
    /// published nor the ones settling what arrived. They stand past the answered heads, so the
    /// next [`Self::document_writes`] carries them; a change built on one its writer never sent
    /// is one no holder can merge.
    ///
    /// # Errors
    /// What arrived does not load; what loaded before it is kept.
    pub fn take_published_documents(
        &mut self,
        arrived: Vec<(TreeChunk, Vec<u8>)>,
        published: &BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
    ) -> Result<BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>, automerge::AutomergeError> {
        let before = self.doc.heads();
        self.take_documents(arrived)?;
        let actor = self.doc.index().get_actor().clone();
        let mut holders = BTreeMap::new();
        for (chunk, doc) in self.doc.each_mut() {
            let mut held = |hashes: Option<&Vec<automerge::ChangeHash>>| {
                hashes
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|hash| doc.has_change(hash))
                    .collect::<Vec<_>>()
            };
            let (before, mut heads) = (held(before.get(&chunk)), held(published.get(&chunk)));
            let mut built_on = HashSet::new();
            for change in doc.get_changes(&before) {
                if change.actor_id() == &actor {
                    continue;
                }
                built_on.extend(change.deps().iter().copied());
                heads.push(change.hash());
            }
            heads.retain(|hash| !built_on.contains(hash));
            heads.sort_unstable();
            heads.dedup();
            holders.insert(chunk, heads);
        }
        Ok(holders)
    }

    /// Where each record of the tree stands before a change: what [`Self::document_writes`]
    /// answers the change against.
    pub fn before_change(&mut self) -> BTreeMap<TreeChunk, Vec<automerge::ChangeHash>> {
        self.document_heads()
    }

    /// What each record holds beyond `known`, the heads its holder has of each record (none of
    /// a record it holds nothing of): one write per record that changed, with the heads it
    /// carries the record to.
    pub fn document_writes(
        &mut self,
        known: &BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
    ) -> Vec<ResourceWrite> {
        self.name_records();
        let mut writes = Vec::new();
        for (chunk, doc) in self.doc.each_mut() {
            let held = known.get(&chunk).map_or(&[][..], Vec::as_slice);
            writes.extend(write_after(chunk, doc, held));
        }
        writes
    }

    /// What record `chunk` holds beyond `held`, the heads its holder has of it, at most about
    /// `budget` bytes of it: the changes after `held` in causal order, as many as fit (one at
    /// least), with the heads the record stands at with them. `None` when it holds nothing
    /// more, or this namespace holds nothing of the record. A record that changed more than
    /// one write carries goes out over several, each building on the last. Reading changes
    /// walks the record's document, so a caller asks only for a record it can send now.
    ///
    /// `held` may name changes this record builds on (heads the holder had before others came
    /// after them): once every change after `held` goes out, the heads are this record's own,
    /// so the holder counts as having all of it rather than heads it can never match.
    pub fn document_write(
        &mut self,
        chunk: TreeChunk,
        held: &[automerge::ChangeHash],
        budget: usize,
    ) -> Option<ResourceWrite> {
        let (_, doc) = self.doc.each_mut().find(|(each, _)| *each == chunk)?;
        if doc.get_heads().as_slice() == held {
            return None;
        }
        let known: Vec<automerge::ChangeHash> = held
            .iter()
            .copied()
            .filter(|hash| doc.has_change(hash))
            .collect();
        let mut changes = Vec::new();
        let mut reached: std::collections::BTreeSet<automerge::ChangeHash> =
            known.iter().copied().collect();
        let mut whole = true;
        for change in doc.get_changes(&known) {
            let raw = change.raw_bytes();
            if !changes.is_empty() && changes.len().saturating_add(raw.len()) > budget {
                whole = false;
                break;
            }
            changes.extend_from_slice(raw);
            for dep in change.deps() {
                reached.remove(dep);
            }
            reached.insert(change.hash());
        }
        let heads = if whole {
            doc.get_heads()
        } else {
            reached.into_iter().collect()
        };
        Some(ResourceWrite {
            chunk,
            changes,
            heads,
        })
    }

    /// Load each of `records` into its document, making a chunk this namespace held nothing
    /// of; answers the nodes they touched, each located in its chunk.
    fn load_records(
        &mut self,
        records: Vec<(TreeChunk, Vec<u8>)>,
    ) -> Result<HashSet<String>, automerge::AutomergeError> {
        let mut touched = HashSet::new();
        for (chunk, bytes) in records {
            if bytes.is_empty() {
                continue;
            }
            if chunk == 0 {
                let doc = self.doc.index_mut();
                // What this device changed and has not named yet is named before a merge settles
                // it: a device signs only the changes that name what made them.
                doc.commit_named();
                let before = doc.get_heads();
                doc.load_incoming(&bytes)?;
                if doc.get_heads() != before {
                    // The reserved root id answers to the tree's own record.
                    touched.insert(ROOT_ID.to_string());
                }
                continue;
            }
            let held = self.doc.chunk_or_new(chunk, false)?;
            let nodes = held.nodes.clone();
            held.doc.commit_named();
            held.doc.arm_patches();
            if let Err(error) = held.doc.load_incoming(&bytes) {
                held.doc.disarm_patches();
                return Err(error);
            }
            let patches = held.doc.take_patches();
            held.doc.disarm_patches();
            let ids = patched_nodes(&nodes, &patches);
            drop(patches);
            for id in ids {
                self.doc.locate(chunk, &id);
                touched.insert(id);
            }
        }
        Ok(touched)
    }

    /// The changes the call `invocation` made to the tree, each with the chunk it is in, oldest
    /// first: one step of whoever made it, which an undo takes back in every document of the
    /// tree it wrote, as the step of a chunked app document is.
    pub fn changes_of_invocation(
        &mut self,
        invocation: tilo_actions::InvocationId,
    ) -> Vec<(TreeChunk, automerge::ChangeHash)> {
        let mut changes: Vec<(i64, TreeChunk, automerge::ChangeHash)> = Vec::new();
        for (chunk, doc) in self.doc.each_mut() {
            doc.commit_named();
            for change in doc.committed_document().get_changes_meta(&[]) {
                if change
                    .message
                    .as_deref()
                    .and_then(tilo_actions::ActionContext::from_change_message)
                    .is_some_and(|context| context.source_invocation == Some(invocation))
                {
                    changes.push((change.timestamp, chunk, change.hash));
                }
            }
        }
        changes.sort_unstable_by_key(|(time, chunk, _)| (*time, *chunk));
        changes
            .into_iter()
            .map(|(_, chunk, hash)| (chunk, hash))
            .collect()
    }

    /// Apply `action` (a history verb: undo, redo, restore) to the document of chunk `chunk` as
    /// one change written with `context`, through the executor every resource's actions go
    /// through, and bring what the namespace reads of the tree up to date with what it moved.
    ///
    /// # Errors
    /// The namespace holds no such chunk, or the action refused.
    pub fn apply_history(
        &mut self,
        chunk: TreeChunk,
        (action, encoded): (&dyn tilo_actions::Action, tilo_actions::EncodedAction),
        context: tilo_actions::ActionContext,
    ) -> Result<(), tilo_actions::ActionError> {
        let nodes = self.doc.chunk(chunk).map(|held| held.nodes.clone());
        let doc = self
            .doc
            .doc_mut(chunk)
            .ok_or_else(|| tilo_actions::ActionError::Refused(format!("no chunk {chunk}")))?;
        doc.commit_named();
        let actor = doc.get_actor().to_bytes().to_vec();
        doc.arm_patches();
        let applied =
            tilo_actions::apply_local_with_context(doc, action, encoded, Some(&actor), context);
        let patches = doc.take_patches();
        doc.disarm_patches();
        applied?;
        let touched = match &nodes {
            Some(nodes) => patched_nodes(nodes, &patches),
            None => HashSet::from([ROOT_ID.to_string()]),
        };
        for id in &touched {
            self.doc.locate(chunk, id);
        }
        self.index_after_apply(&touched);
        Ok(())
    }

    /// Whether this namespace holds a tree of its own, rather than only the empty maps a
    /// fresh document starts with.
    #[must_use]
    pub fn holds_tree(&self) -> bool {
        self.doc.node_ids().next().is_some()
    }

    /// Name every record of this mount's tree as what it is, when the mount holds a workspace's
    /// tree: the id, kind, home and workspace a reader checks before it opens a record. A mount
    /// that founds a tree names it before it publishes anything, so a page's Files opens what the
    /// mount wrote; a record that already names itself is left as it is.
    fn name_records(&mut self) {
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        let chunks: Vec<TreeChunk> = self.doc.records().collect();
        for chunk in chunks {
            self.name_record(chunk, &workspace);
        }
    }

    /// Name the record `chunk` of `workspace`'s tree as what it is: the id, kind, home and
    /// workspace every reader checks before it opens a record, stamped into the document this
    /// mount publishes. A mount that founds a tree no page would open is one the page's Files
    /// cannot show ("record identity does not match its binding"). Whether the document names
    /// itself afterwards, already or by this stamp.
    fn name_record(&mut self, chunk: TreeChunk, workspace: &str) -> bool {
        let Some(id) = tree_document(chunk) else {
            return false;
        };
        let Some(doc) = self.doc.doc_mut(chunk) else {
            return false;
        };
        doc.commit_named();
        // A record a build before wrote names nothing: it takes the founding every holder
        // derives alike, which the workspace's server gives it too, so the two identities merge
        // as one instead of competing.
        if let Some(mut founding) = collab_types::document::identity::founding_for_unnamed(
            doc,
            &id,
            &tree_kind(),
            workspace,
        ) && (**doc).merge_incoming(&mut founding).is_err()
        {
            return false;
        }
        collab_types::document::identity::init_scoped(
            doc,
            &id,
            &tree_kind(),
            workspace,
            Some(workspace),
        )
    }
}

/// Document `chunk` of a workspace's tree, as the resource that holds it is named.
fn tree_document(chunk: TreeChunk) -> Option<collab_types::document::DocumentId> {
    collab_types::document::DocumentId::parse(&fs_protocol::tree::chunk_document_id(chunk))
}

/// The kind every document of a workspace's tree is: Files' tree.
fn tree_kind() -> collab_types::document::ResourceType {
    collab_types::document::ResourceType::new(fs_protocol::tree::TREE_KIND)
}

/// The document that founds `chunk` of `workspace`'s tree: the identity every founder of it
/// derives alike, so a page's Files that started the tree and a mount that starts it as it writes
/// the first file make one document. A founder with a genesis of its own writes the identity a
/// second time, and the merged document holds none at all. `None` where the document cannot be
/// named.
pub(super) fn founding_document(workspace: &str, chunk: TreeChunk) -> Option<AutoCommit> {
    let id = tree_document(chunk)?;
    collab_types::document::identity::founding(&id, &tree_kind(), workspace, workspace)
}

/// The nodes of the map `nodes` that `patches` touched: those put or deleted in it, and those
/// any object below them changed in.
fn patched_nodes(nodes: &ObjId, patches: &[automerge::Patch]) -> HashSet<String> {
    let mut ids = HashSet::new();
    for patch in patches {
        let key = if patch.obj == *nodes {
            match &patch.action {
                automerge::PatchAction::PutMap { key, .. }
                | automerge::PatchAction::DeleteMap { key } => Some(key),
                _ => None,
            }
        } else {
            patch.path.iter().find_map(|(parent, prop)| match prop {
                automerge::Prop::Map(key) if parent == nodes => Some(key),
                _ => None,
            })
        };
        if let Some(key) = key {
            ids.insert(key.clone());
        }
    }
    ids
}

/// What `doc`, record `chunk`, holds beyond `held`: `None` when nothing.
fn write_after(
    chunk: TreeChunk,
    doc: &mut ChangeTrackedDoc,
    held: &[automerge::ChangeHash],
) -> Option<ResourceWrite> {
    let heads = doc.get_heads();
    if heads.as_slice() == held {
        return None;
    }
    let held: Vec<automerge::ChangeHash> = held
        .iter()
        .copied()
        .filter(|hash| doc.has_change(hash))
        .collect();
    let changes = doc.save_after(&held);
    Some(ResourceWrite {
        chunk,
        changes,
        heads,
    })
}

/// One record's write: what record `chunk` of the tree (0 for the tree's own) holds beyond
/// what its holder had, and the heads it carries the record to ([`Namespace::document_writes`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceWrite {
    pub chunk: TreeChunk,
    /// The changes, as one incremental save; empty when the holder already has them all.
    pub changes: Vec<u8>,
    pub heads: Vec<automerge::ChangeHash>,
}

/// What a namespace's records are kept or sent as together ([`Namespace::save`],
/// [`Namespace::save_incremental`]): this tag, then each record's number and length (each
/// four bytes, little-endian) and its bytes.
const BUNDLE_TAG: &[u8; 8] = b"tilotree";

/// `records` kept as one; empty when every record is.
pub(super) fn bundle(records: Vec<(TreeChunk, Vec<u8>)>) -> Vec<u8> {
    if records.iter().all(|(_, bytes)| bytes.is_empty()) {
        return Vec::new();
    }
    let mut out = BUNDLE_TAG.to_vec();
    for (chunk, bytes) in records {
        if bytes.is_empty() {
            continue;
        }
        out.extend_from_slice(&chunk.to_le_bytes());
        out.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(&bytes);
    }
    out
}

/// The records [`bundle`] kept: none for nothing.
fn unbundle(bytes: &[u8]) -> Result<Vec<(TreeChunk, Vec<u8>)>, automerge::AutomergeError> {
    let malformed = || automerge::AutomergeError::InvalidObjId("not a namespace's records".into());
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    let mut rest = bytes
        .strip_prefix(BUNDLE_TAG.as_slice())
        .ok_or_else(malformed)?;
    let mut records = Vec::new();
    while !rest.is_empty() {
        let (head, tail) = rest.split_at_checked(8).ok_or_else(malformed)?;
        let (chunk, len) = head.split_at(4);
        let chunk = TreeChunk::from_le_bytes(chunk.try_into().map_err(|_| malformed())?);
        let len = u32::from_le_bytes(len.try_into().map_err(|_| malformed())?) as usize;
        let (record, tail) = tail.split_at_checked(len).ok_or_else(malformed)?;
        records.push((chunk, record.to_vec()));
        rest = tail;
    }
    Ok(records)
}
