//! The namespace as the records the tree is kept in: what a device holding the tree reads
//! from them, takes in from them and writes to them, and what it keeps of itself on disk.

use std::collections::{BTreeMap, HashSet};

use automerge::{AutoCommit, ObjId};
use fs_protocol::TreeChunk;
use tilo_automerge::ApplyIncoming as _;

use super::changes::ChangeTrackedDoc;
use super::chunks::Chunks;
use super::{Namespace, ROOT_ID, ROOT_INO, ROOT_DOCUMENT_ID_KEY, next_merge_generation, str_at_doc};

impl Namespace {
    /// A namespace as [`Self::save`] kept it.
    ///
    /// # Errors
    /// `data` is not what [`Self::save`] writes, or a record in it is no document.
    pub fn from_bytes(
        author: impl Into<String>,
        data: &[u8],
    ) -> Result<Self, automerge::AutomergeError> {
        Self::from_records(author, unbundle(data)?)
    }

    /// A namespace holding the tree's records `records`, each the number of its chunk (0 for
    /// the tree's own record) and its document saved whole: what a device that holds the
    /// tree's records starts from.
    ///
    /// # Errors
    /// A record is no document.
    pub fn from_records(
        author: impl Into<String>,
        records: impl IntoIterator<Item = (TreeChunk, Vec<u8>)>,
    ) -> Result<Self, automerge::AutomergeError> {
        let mut index = None;
        let mut chunks = Vec::new();
        for (chunk, bytes) in records {
            let doc = AutoCommit::load(&bytes)?;
            if chunk == 0 {
                index = Some(doc);
            } else {
                chunks.push((chunk, doc));
            }
        }
        let mut docs = Chunks::new(index.unwrap_or_default());
        for (chunk, doc) in chunks {
            docs.adopt_chunk(chunk, doc)?;
        }
        docs.locate_all();
        let author = author.into();
        // Resume the id counter past ids we authored before, so a restart can't reuse a node id.
        let prefix = format!("{author}/");
        let max_local = docs
            .node_ids()
            .filter_map(|id| id.strip_prefix(&prefix)?.parse::<u64>().ok())
            .map(|n| n.saturating_add(1))
            .max()
            .unwrap_or(0);
        let mut ns = Self::over(docs, author, max_local);
        ns.ensure_document_ids();
        ns.rebuild_document_index();
        // Name what opening wrote as the mount's now: a merge made before the next save settles
        // a pending change without its name, and a device signs only the changes that name what
        // made them.
        for (_, doc) in ns.doc.each_mut() {
            doc.commit_named();
        }
        Ok(ns)
    }

    /// Every record of the tree, kept as one: what [`Self::from_bytes`] reads back.
    pub fn save(&mut self) -> Vec<u8> {
        self.note_change();
        bundle(
            self.doc
                .each_mut()
                .map(|(chunk, doc)| (chunk, doc.save()))
                .collect(),
        )
    }

    /// Record `chunk` of the tree (0 for the tree's own) saved whole, if this namespace holds it.
    pub fn save_record(&mut self, chunk: TreeChunk) -> Option<Vec<u8>> {
        self.note_change();
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

    /// Finish a mutation: every node it made has its stable record id before anything
    /// reads the document (`save`, `changes_since`).
    ///
    /// INVARIANT: pair every mutation with its `note_change` inside the same `Namespace`
    /// lock scope, so the ids of a change are there before another change's `save` folds
    /// them.
    pub fn note_change(&mut self) {
        if !self.document_ids_complete {
            self.ensure_document_ids();
        }
    }

    /// Changes since the last `save_incremental`/load/merge, advancing the shared
    /// automerge watermark.
    ///
    /// INVARIANT: pair every mutation with its `save_incremental` inside the same
    /// `Namespace` lock scope: the watermark is one shared cursor, so a mutation saved
    /// outside that scope can have its delta swallowed by a concurrent remote apply.
    pub fn save_incremental(&mut self) -> Vec<u8> {
        self.note_change();
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
    /// workspace keeps ([`Self::take_records`]), and return what resolving them changed.
    pub fn adopt_or_merge_remote_and_resolve(
        &mut self,
        data: &[u8],
    ) -> Result<Vec<u8>, automerge::AutomergeError> {
        let records = unbundle(data)?;
        for (_, doc) in self.doc.each_mut() {
            let _ = doc.save_incremental();
        }
        self.take_records(records)?;
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
    pub fn take_records(
        &mut self,
        records: Vec<(TreeChunk, Vec<u8>)>,
    ) -> Result<(), automerge::AutomergeError> {
        self.take_records_touching(records).map(drop)
    }

    /// [`Self::take_records`], answering the nodes the records touched: what a follower of the
    /// tree reads again, rather than every node. A namespace that adopted the tree's own record
    /// answers every node.
    ///
    /// # Errors
    /// A record does not load; what loaded before it is kept.
    pub fn take_records_touching(
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
                self.document_ids_complete = false;
                self.merge_generation = next_merge_generation();
                self.cycle_scan_pending = true;
                self.placed_since_resolve.clear();
                let touched = self.load_records(records)?;
                drop(touched);
                self.ensure_document_ids();
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
    pub fn record_heads(&mut self) -> BTreeMap<TreeChunk, Vec<automerge::ChangeHash>> {
        self.note_change();
        self.doc.heads()
    }

    /// Whether record `chunk` holds the change `hash`: what a change delivered before the record it
    /// builds on finds missing.
    pub fn holds_change(&mut self, chunk: TreeChunk, hash: &automerge::ChangeHash) -> bool {
        self.doc
            .doc_mut(chunk)
            .is_some_and(|doc| doc.has_change(hash))
    }

    /// Take in the records `records` ([`Self::take_records`]) and answer what settling them wrote,
    /// one write per record: this device's own changes repairing a conflict the records brought,
    /// which go out on their own so every holder of the tree settles it alike. Empty when nothing
    /// needed settling.
    ///
    /// The changes taken in are not this device's to publish: only the changes this namespace
    /// authored beyond what its records held before are answered.
    ///
    /// # Errors
    /// A record does not load; what loaded before it is kept.
    pub fn take_records_and_resolve(
        &mut self,
        records: Vec<(TreeChunk, Vec<u8>)>,
    ) -> Result<Vec<ResourceWrite>, automerge::AutomergeError> {
        let before = self.record_heads();
        self.take_records(records)?;
        let actor = self.doc.index().get_actor().clone();
        let mut writes = Vec::new();
        for (chunk, doc) in self.doc.each_mut() {
            let heads = doc.get_heads();
            let held: Vec<automerge::ChangeHash> = before
                .get(&chunk)
                .map(|held| {
                    held.iter()
                        .copied()
                        .filter(|hash| doc.has_change(hash))
                        .collect()
                })
                .unwrap_or_default();
            if heads == held {
                continue;
            }
            let mut changes = Vec::new();
            for change in doc.get_changes(&held) {
                if change.actor_id() != &actor {
                    continue;
                }
                changes.extend_from_slice(change.raw_bytes());
            }
            if changes.is_empty() {
                continue;
            }
            writes.push(ResourceWrite {
                chunk,
                changes,
                heads,
            });
        }
        Ok(writes)
    }

    /// What each record holds beyond `known`, the heads its holder has of each record (none of
    /// a record it holds nothing of): one write per record that changed, with the heads it
    /// carries the record to.
    pub fn document_writes(
        &mut self,
        known: &BTreeMap<TreeChunk, Vec<automerge::ChangeHash>>,
    ) -> Vec<ResourceWrite> {
        self.note_change();
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
    pub fn record_write(
        &mut self,
        chunk: TreeChunk,
        held: &[automerge::ChangeHash],
        budget: usize,
    ) -> Option<ResourceWrite> {
        self.note_change();
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
        for change in doc.get_changes(&known) {
            let raw = change.raw_bytes();
            if !changes.is_empty() && changes.len().saturating_add(raw.len()) > budget {
                break;
            }
            changes.extend_from_slice(raw);
            for dep in change.deps() {
                reached.remove(dep);
            }
            reached.insert(change.hash());
        }
        Some(ResourceWrite {
            chunk,
            changes,
            heads: reached.into_iter().collect(),
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
                    self.merge_generation = next_merge_generation();
                    // The reserved root id answers to the tree's own record.
                    touched.insert(ROOT_ID.to_string());
                }
                continue;
            }
            let held = self.doc.chunk_or_new(chunk, false)?;
            let nodes = held.nodes.clone();
            held.doc.commit_named();
            let before = held.doc.get_heads();
            held.doc.arm_patches();
            if let Err(error) = held.doc.load_incoming(&bytes) {
                held.doc.disarm_patches();
                return Err(error);
            }
            let patches = held.doc.take_patches();
            held.doc.disarm_patches();
            let moved = held.doc.get_heads() != before;
            let ids = patched_nodes(&nodes, &patches);
            drop(patches);
            if moved {
                self.merge_generation = next_merge_generation();
            }
            for id in ids {
                self.doc.locate(chunk, &id);
                touched.insert(id);
            }
        }
        Ok(touched)
    }

    /// Whether this namespace holds a tree of its own, rather than only the empty maps a
    /// fresh document starts with.
    #[must_use]
    pub fn holds_tree(&self) -> bool {
        self.doc.node_ids().next().is_some()
    }
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
