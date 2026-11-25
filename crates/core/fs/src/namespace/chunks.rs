//! The namespace's documents: the tree's own record and its chunks.
//!
//! A workspace's tree is kept as several records ([`fs_protocol::tree::CHUNK_NODES`]): the
//! tree's own record, chunk 0, holds the root's identity, the chunks it has and each device's
//! changeset marks; every other chunk holds the nodes made in it, with their histories,
//! attributes and leases. A node lives in the chunk its record id names and never moves, so
//! every change of a node is a change of one chunk, and a holder of the tree stores, seals and
//! sends what that chunk changed. A namespace holds every chunk of its tree.
//!
//! [`Chunks`] routes a read or a write to the document of the chunk it names: an object of the
//! tree is an [`Obj`], a chunk and an object of that chunk's document.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, AutomergeError, ObjId, ObjType, Prop, ROOT, ReadDoc as _, Value};
use fs_protocol::TreeChunk;

use super::changes::ChangeTrackedDoc;

/// An object of the tree: which chunk's document, and the object there. Chunk 0 is the
/// tree's own record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Obj {
    pub chunk: TreeChunk,
    pub id: ObjId,
}

impl Obj {
    /// The root of chunk `chunk`'s document.
    pub(crate) const fn root(chunk: TreeChunk) -> Self {
        Self { chunk, id: ROOT }
    }

    /// The root of the tree's own record.
    pub(crate) const fn index() -> Self {
        Self::root(0)
    }
}

impl std::fmt::Display for Obj {
    /// The object's id within its document: what an operation's id reads as.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.id.fmt(f)
    }
}

/// One chunk's document and its map of nodes.
#[derive(Clone)]
pub(super) struct Chunk {
    pub(super) doc: ChangeTrackedDoc,
    pub(super) nodes: ObjId,
}

/// The tree's own record and its chunks, each a document of its own.
pub(crate) struct Chunks {
    index: ChangeTrackedDoc,
    held: BTreeMap<TreeChunk, Chunk>,
    /// The chunk each node lives in, as the chunks hold them.
    located: HashMap<String, TreeChunk>,
    /// The last chunk whose node count was asked for, and the count, packed in one word: making
    /// a node asks the newest chunk how full it is, and counting it in the document walks every
    /// node it holds, so a create that asked again would cost the chunk per file. One word
    /// written atomically, because a namespace is shared by reference across the mount's threads
    /// and the answer is only a cache of what the document already holds.
    counted: AtomicU64,
}

/// What [`Chunks::counted`] holds when no chunk's count is kept.
const NOT_COUNTED: u64 = u64::MAX;

/// The count of `chunk` as the one word [`Chunks::counted`] holds.
fn counted_word(chunk: TreeChunk, count: usize) -> u64 {
    (u64::from(chunk) << 32) | u64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// The chunk and count [`Chunks::counted`] holds, `None` when it holds none.
fn counted_value(word: u64) -> Option<(TreeChunk, usize)> {
    if word == NOT_COUNTED {
        return None;
    }
    let chunk = TreeChunk::try_from(word >> 32).ok()?;
    let count = usize::try_from(word & u64::from(u32::MAX)).ok()?;
    Some((chunk, count))
}

impl Clone for Chunks {
    fn clone(&self) -> Self {
        Self {
            index: self.index.clone(),
            held: self.held.clone(),
            located: self.located.clone(),
            counted: AtomicU64::new(self.counted.load(Ordering::Relaxed)),
        }
    }
}

/// Make `doc` a chunk of the tree: its map of nodes, which the record's founding stood up.
fn chunk_maps(doc: &mut AutoCommit) -> Result<ObjId, AutomergeError> {
    fs_protocol::tree::tree_nodes(doc)
}

impl Chunks {
    pub(super) fn new(index: AutoCommit, person: Option<tilo_actions::MemberId>) -> Self {
        Self {
            index: ChangeTrackedDoc::new(index, person),
            held: BTreeMap::new(),
            located: HashMap::new(),
            counted: AtomicU64::new(NOT_COUNTED),
        }
    }

    /// The document of chunk `chunk`: the tree's own record for 0.
    pub(super) fn doc(&self, chunk: TreeChunk) -> Option<&ChangeTrackedDoc> {
        if chunk == 0 {
            return Some(&self.index);
        }
        self.held.get(&chunk).map(|chunk| &chunk.doc)
    }

    /// [`Self::doc`], to change.
    pub(super) fn doc_mut(&mut self, chunk: TreeChunk) -> Option<&mut ChangeTrackedDoc> {
        if chunk == 0 {
            return Some(&mut self.index);
        }
        self.held.get_mut(&chunk).map(|chunk| &mut chunk.doc)
    }

    pub(super) fn index(&self) -> &ChangeTrackedDoc {
        &self.index
    }

    pub(super) fn index_mut(&mut self) -> &mut ChangeTrackedDoc {
        &mut self.index
    }

    /// The chunks that hold nodes, in order.
    pub(super) fn chunk_numbers(&self) -> Vec<TreeChunk> {
        self.held.keys().copied().collect()
    }

    /// Every record of the tree: the tree's own, then each chunk.
    pub(super) fn records(&self) -> impl Iterator<Item = TreeChunk> + '_ {
        std::iter::once(0).chain(self.held.keys().copied())
    }

    pub(super) fn chunk(&self, chunk: TreeChunk) -> Option<&Chunk> {
        self.held.get(&chunk)
    }

    /// The chunk `chunk`, made when this namespace holds none of it yet; listed in the tree's
    /// own record when `list`, as a chunk this device puts nodes in is.
    pub(super) fn chunk_or_new(
        &mut self,
        chunk: TreeChunk,
        list: bool,
    ) -> Result<&mut Chunk, AutomergeError> {
        if chunk == 0 {
            return Err(AutomergeError::InvalidObjId(
                "the tree's own record holds no nodes".into(),
            ));
        }
        if !self.held.contains_key(&chunk) {
            let mut doc = super::started_from(super::founding_document(chunk));
            doc.set_actor(self.index.get_actor().clone());
            let nodes = chunk_maps(&mut doc)?;
            self.held.insert(
                chunk,
                Chunk {
                    doc: ChangeTrackedDoc::sharing(doc, &self.index),
                    nodes,
                },
            );
        }
        if list && !fs_protocol::tree::listed_chunks(&*self.index).contains(&chunk) {
            // Listing a chunk writes the tree's own record's `chunks` map, which its founding
            // stood up: this device's pending edits are named as the mount's first, so the
            // listing is the one change it is.
            self.index.commit_named();
            fs_protocol::tree::list_chunk(&mut self.index, chunk)?;
        }
        self.held
            .get_mut(&chunk)
            .ok_or_else(|| AutomergeError::InvalidObjId("chunk".into()))
    }

    /// Take `doc` in as chunk `chunk`'s document whole, in place of what this namespace held of
    /// it: the nodes it holds are located there.
    pub(super) fn adopt_chunk(
        &mut self,
        chunk: TreeChunk,
        mut doc: AutoCommit,
    ) -> Result<(), AutomergeError> {
        doc.set_actor(self.index.get_actor().clone());
        self.forget_node_count();
        if chunk == 0 {
            self.index.replace_document(doc);
            return Ok(());
        }
        let nodes = chunk_maps(&mut doc)?;
        if let Some(old) = self.held.remove(&chunk) {
            for id in old.doc.keys(&old.nodes) {
                self.located.remove(&id);
            }
        }
        for id in doc.keys(&nodes) {
            self.located.insert(id, chunk);
        }
        self.held.insert(
            chunk,
            Chunk {
                doc: ChangeTrackedDoc::sharing(doc, &self.index),
                nodes,
            },
        );
        Ok(())
    }

    /// Where the node `id` lives now, as the chunk `chunk` reads: what a change to that chunk
    /// moves.
    pub(super) fn locate(&mut self, chunk: TreeChunk, id: &str) {
        self.forget_node_count();
        let held = self
            .held
            .get(&chunk)
            .is_some_and(|held| map_object(&held.doc, &held.nodes, id).is_some());
        if held {
            self.located.insert(id.to_owned(), chunk);
        } else if self.located.get(id) == Some(&chunk) {
            self.located.remove(id);
        }
    }

    /// Locate every node every chunk holds: what opening a namespace does.
    pub(super) fn locate_all(&mut self) {
        self.forget_node_count();
        self.located.clear();
        for (&chunk, held) in &self.held {
            for id in held.doc.keys(&held.nodes) {
                self.located.insert(id, chunk);
            }
        }
    }

    /// The node `id`'s object.
    pub(super) fn node(&self, id: &str) -> Option<Obj> {
        let chunk = *self.located.get(id)?;
        let held = self.held.get(&chunk)?;
        map_object(&held.doc, &held.nodes, id).map(|id| Obj { chunk, id })
    }

    /// Make the node `id` in chunk `chunk`, an empty map: what a test that builds a tree by
    /// hand writes.
    #[cfg(test)]
    pub(super) fn put_node(&mut self, chunk: TreeChunk, id: &str) -> Result<Obj, AutomergeError> {
        let held = self.chunk_or_new(chunk, true)?;
        let nodes = held.nodes.clone();
        let made = held.doc.put_object(&nodes, id, ObjType::Map)?;
        self.forget_node_count();
        self.located.insert(id.to_owned(), chunk);
        Ok(Obj { chunk, id: made })
    }

    /// Write the file action `action` done by `invoker` into the document of the chunk its
    /// node lives in: for a node it makes, the chunk its document id names, made and listed
    /// when this namespace holds none of it yet.
    pub(super) fn write_action(
        &mut self,
        action: &fs_protocol::FileAction,
        ambient: Option<&tilo_actions::ActionContext>,
    ) -> Result<(), tilo_actions::ActionError> {
        let node = action.node();
        let chunk = match action {
            fs_protocol::FileAction::Create { document_id, .. } => {
                let chunk = document_id.chunk();
                self.chunk_or_new(chunk, true)?;
                if self.index.pending_ops() > 0 {
                    // The chunk was listed for this node: that is part of making it.
                    let described = action.described();
                    self.index.name_as(
                        &fs_protocol::tree::document_id(),
                        described.what,
                        node,
                        ambient,
                    );
                }
                chunk
            }
            _ => *self.located.get(node).ok_or_else(|| {
                tilo_actions::ActionError::Refused(format!("the tree holds no node {node}"))
            })?,
        };
        let document = if chunk == 0 {
            fs_protocol::tree::document_id()
        } else {
            fs_protocol::tree::chunk_document_id(chunk)
        };
        let doc = self
            .doc_mut(chunk)
            .ok_or_else(|| tilo_actions::ActionError::Refused(format!("no chunk {chunk}")))?;
        doc.write_action(&document, action, ambient)?;
        self.located.insert(node.to_owned(), chunk);
        // A create is the one action that adds a node to the chunk's map, and the count the
        // next create asks for is this one's plus it.
        if matches!(action, fs_protocol::FileAction::Create { .. })
            && let Some((counted, count)) = counted_value(self.counted.load(Ordering::Relaxed))
            && counted == chunk
        {
            self.counted.store(
                counted_word(chunk, count.saturating_add(1)),
                Ordering::Relaxed,
            );
        }
        Ok(())
    }

    /// Every node id, in order.
    pub(super) fn sorted_node_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.located.keys().cloned().collect();
        ids.sort_unstable();
        ids
    }

    /// Every node id, in no order.
    pub(super) fn node_ids(&self) -> impl Iterator<Item = &String> + '_ {
        self.located.keys()
    }

    /// How many nodes chunk `chunk` holds. Counting it in the document walks every node, so the
    /// answer is kept: placing a node asks the newest chunk's count before every create.
    pub(super) fn nodes_in(&self, chunk: TreeChunk) -> usize {
        if let Some((counted, count)) = counted_value(self.counted.load(Ordering::Relaxed))
            && counted == chunk
        {
            return count;
        }
        let count = self
            .held
            .get(&chunk)
            .map_or(0, |held| held.doc.length(&held.nodes));
        self.counted
            .store(counted_word(chunk, count), Ordering::Relaxed);
        count
    }

    /// The chunk a new node goes in: the newest one this tree holds.
    pub(super) fn newest_chunk(&self) -> Option<TreeChunk> {
        self.held.keys().next_back().copied()
    }

    /// Forget the kept node count: nodes arrived from somewhere other than a create here.
    fn forget_node_count(&self) {
        self.counted.store(NOT_COUNTED, Ordering::Relaxed);
    }

    /// Up to `limit` node ids in key order after `after`, over every chunk.
    pub(super) fn node_ids_after(&self, after: Option<&str>, limit: usize) -> Vec<String> {
        let start = after.map_or(std::ops::Bound::Unbounded, |id| {
            std::ops::Bound::Excluded(id.to_string())
        });
        let mut ids: Vec<String> = self
            .held
            .values()
            .flat_map(|held| {
                held.doc
                    .map_range(&held.nodes, (start.clone(), std::ops::Bound::Unbounded))
                    .take(limit)
                    .map(|item| item.key.into_owned())
                    .collect::<Vec<_>>()
            })
            .collect();
        ids.sort_unstable();
        ids.truncate(limit);
        ids
    }

    pub(super) fn get(
        &self,
        obj: &Obj,
        prop: impl Into<Prop>,
    ) -> Result<Option<(Value<'_>, Obj)>, AutomergeError> {
        let doc = self.doc_of(obj)?;
        Ok(doc.get(&obj.id, prop)?.map(|(value, id)| {
            (
                value,
                Obj {
                    chunk: obj.chunk,
                    id,
                },
            )
        }))
    }

    pub(super) fn get_all(
        &self,
        obj: &Obj,
        prop: impl Into<Prop>,
    ) -> Result<Vec<(Value<'_>, Obj)>, AutomergeError> {
        let doc = self.doc_of(obj)?;
        Ok(doc
            .get_all(&obj.id, prop)?
            .into_iter()
            .map(|(value, id)| {
                (
                    value,
                    Obj {
                        chunk: obj.chunk,
                        id,
                    },
                )
            })
            .collect())
    }

    pub(super) fn keys(&self, obj: &Obj) -> Vec<String> {
        self.doc(obj.chunk)
            .map(|doc| doc.keys(&obj.id).collect())
            .unwrap_or_default()
    }

    pub(super) fn put(
        &mut self,
        obj: &Obj,
        prop: impl Into<Prop>,
        value: impl Into<automerge::ScalarValue>,
    ) -> Result<(), AutomergeError> {
        self.doc_of_mut(obj)?.put(&obj.id, prop, value)
    }

    pub(super) fn delete(
        &mut self,
        obj: &Obj,
        prop: impl Into<Prop>,
    ) -> Result<(), AutomergeError> {
        self.doc_of_mut(obj)?.delete(&obj.id, prop)
    }

    fn doc_of(&self, obj: &Obj) -> Result<&ChangeTrackedDoc, AutomergeError> {
        self.doc(obj.chunk)
            .ok_or_else(|| AutomergeError::InvalidObjId(format!("chunk {}", obj.chunk)))
    }

    fn doc_of_mut(&mut self, obj: &Obj) -> Result<&mut ChangeTrackedDoc, AutomergeError> {
        self.doc_mut(obj.chunk)
            .ok_or_else(|| AutomergeError::InvalidObjId(format!("chunk {}", obj.chunk)))
    }

    /// Each record's heads, the tree's own first.
    pub(super) fn heads(&mut self) -> BTreeMap<TreeChunk, Vec<automerge::ChangeHash>> {
        let mut heads = BTreeMap::from([(0, self.index.get_heads())]);
        for (&chunk, held) in &mut self.held {
            heads.insert(chunk, held.doc.get_heads());
        }
        heads
    }

    /// Every record, mutably: the tree's own first.
    pub(super) fn each_mut(&mut self) -> impl Iterator<Item = (TreeChunk, &mut ChangeTrackedDoc)> {
        std::iter::once((0, &mut self.index)).chain(
            self.held
                .iter_mut()
                .map(|(&chunk, held)| (chunk, &mut held.doc)),
        )
    }

    pub(super) fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.index.subscribe()
    }
}

/// The map object at `key` of `parent` in `doc`.
fn map_object(doc: &AutoCommit, parent: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(parent, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}
