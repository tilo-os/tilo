//! Buffered block writes for whole-document imports (see [`DocMut::bulk`]).
//!
//! Automerge applies every local operation to its op set on the spot, and each
//! application pays for a seek plus an update of every op-set column and index.
//! Built op by op, an imported block costs a dozen of those (the block map, its
//! order, kind, attrs map, text object, each attribute, each text run) and every
//! inline mark costs two more of the most expensive kind: the mark index is
//! rebuilt around each anchor. Inside a bulk scope new blocks and all marks live
//! in memory until the scope ends and are then written together, in one of two
//! ways:
//!
//! - A document whose owner commits freely, or one being created, receives them
//!   as the ops of new changes. Automerge integrates a change's ops
//!   on objects it has not seen as a single run at the end of its op set, marks
//!   included.
//! - A host-owned document with history keeps the host's transaction: each block goes in as
//!   one [`Transactable::batch_create_object`] call (one op-set splice for the
//!   whole block tree), and marks follow as local operations once all text is in
//!   place.

use super::{BLOCK_ATTRS, BLOCK_KIND, BLOCK_ORDER, BLOCK_TEXT, DocMut, ObjType, Value, block_obj};
use automerge::legacy;
use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, TextEncoding, hydrate};
use std::collections::{BTreeMap, HashMap, HashSet};

#[cfg(test)]
thread_local! {
    /// Makes [`DocMut::bulk`] a plain call, so tests can build the op-by-op reference document.
    static OP_BY_OP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// How many flushes on this thread wrote their blocks as changes.
    static CHANGE_WRITES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many bulk flushes on this thread wrote their blocks as changes (not as local ops).
#[cfg(test)]
pub(crate) fn change_writes() -> usize {
    CHANGE_WRITES.with(std::cell::Cell::get)
}

/// Run `f` with every bulk scope on this thread disabled (the op-by-op reference path).
#[cfg(test)]
pub(crate) fn op_by_op<R>(f: impl FnOnce() -> R) -> R {
    OP_BY_OP.with(|c| c.set(true));
    let out = f();
    OP_BY_OP.with(|c| c.set(false));
    out
}

/// A block created inside a bulk scope, held in memory until the scope ends.
struct StagedBlock {
    container: ObjId,
    id: String,
    order: Vec<u8>,
    kind: String,
    attrs: BTreeMap<String, ScalarValue>,
    /// `None` until something writes text or a mark, mirroring the lazily created text object.
    text: Option<String>,
    /// Length of `text` in characters, the unit automerge text indices count in here.
    text_len: usize,
}

impl StagedBlock {
    /// The block as the tree the op-by-op path would have built.
    fn into_value(self, encoding: TextEncoding) -> hydrate::Value {
        let attrs: HashMap<String, hydrate::Value> = self
            .attrs
            .into_iter()
            .map(|(k, v)| (k, hydrate::Value::Scalar(v)))
            .collect();
        let mut block: HashMap<&str, hydrate::Value> = HashMap::with_capacity(4);
        block.insert(
            BLOCK_ORDER,
            hydrate::Value::scalar(ScalarValue::Bytes(self.order)),
        );
        block.insert(BLOCK_KIND, hydrate::Value::scalar(self.kind));
        block.insert(BLOCK_ATTRS, hydrate::Value::Map(attrs.into()));
        if let Some(text) = &self.text {
            block.insert(BLOCK_TEXT, hydrate::Value::text(encoding, text));
        }
        hydrate::Value::Map(block.into())
    }
}

/// A mark added inside a bulk scope, applied when the scope ends.
struct StagedMark {
    container: ObjId,
    block: String,
    start: usize,
    end: usize,
    name: String,
    value: ScalarValue,
}

/// The staging area of an open bulk scope. It rides on [`super::IdGen`], which already
/// threads through every [`DocMut`] over the same document.
#[derive(Default)]
pub(super) struct Bulk {
    depth: usize,
    /// Staged blocks in creation order; a removed block leaves `None`.
    blocks: Vec<Option<StagedBlock>>,
    by_id: HashMap<String, usize>,
    marks: Vec<StagedMark>,
    /// Blocks with a staged mark. Splicing into one of them first applies the marks, so a mark
    /// expands over later text exactly as it would have had it been applied at once.
    marked: HashSet<String>,
    /// Flushes in the open scope may write as changes; decided when the outermost scope opens.
    may_commit: bool,
}

impl DocMut<'_> {
    /// Run `f` with block creation buffered: blocks inserted inside the scope — with their kind,
    /// attributes and appended text — are written together when the outermost scope ends, and
    /// marks added inside the scope are applied after all text is in place.
    ///
    /// The resulting document content is the same as without the scope. Operations that read or
    /// restructure existing blocks flush the buffer first, so they observe every staged block.
    /// Reads that bypass `DocMut` (the free functions over `doc`) do not see staged blocks until
    /// the scope ends; use [`Self::block_text_len`] for the one read an importer needs.
    ///
    /// When bulk writes may commit, the scope ends by committing and writing the staged blocks and
    /// their marks as new changes. They may for an owned [`crate::Document`] (see
    /// [`super::IdGen::allow_commits`]), and for a document being created: when the scope opens it
    /// holds nothing but the schema, and the generator came from [`super::IdGen::new`] rather than
    /// from the document (a host that just made a document to open a file into). Nothing can refer
    /// to such a document's history yet: no peer has seen it, and an undo log bracketing actions
    /// by heads spans every change up to its next boundary whether the import committed or not.
    /// Code editing an existing document seeds its generator from it
    /// ([`super::seeded_id_gen`]), so an action replayed on a new, empty document still writes
    /// within its own transaction. A host's document written this way keeps its last block in the
    /// host's open transaction, so the host's next commit still carries the import and brackets
    /// all of it as one undoable action.
    ///
    /// Otherwise a host-owned document keeps the host's transaction boundary: blocks go in as
    /// batch objects and marks as ordinary local operations.
    pub fn bulk<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        #[cfg(test)]
        if OP_BY_OP.with(std::cell::Cell::get) {
            return f(self);
        }
        if self.ids.bulk.depth == 0 {
            self.ids.bulk.may_commit = self.ids.commits_allowed
                || (self.ids.unseeded && super::holds_only_schema(self.doc));
        }
        self.ids.bulk.depth = self.ids.bulk.depth.saturating_add(1);
        let out = f(self);
        self.ids.bulk.depth = self.ids.bulk.depth.saturating_sub(1);
        if self.ids.bulk.depth == 0 {
            self.flush_bulk();
            self.ids.bulk.may_commit = false;
        }
        out
    }

    pub(super) fn bulk_active(&self) -> bool {
        self.ids.bulk.depth > 0
    }

    /// Stage a new block in `container`, ordered after `after`; returns its id.
    pub(super) fn stage_block(
        &mut self,
        container: ObjId,
        after: Option<&str>,
        kind: &str,
    ) -> String {
        let order = self.order_after_in(&container, after);
        let id = self.ids.mint();
        self.record_order(&container, &id, &order);
        let bulk = &mut self.ids.bulk;
        bulk.by_id.insert(id.clone(), bulk.blocks.len());
        bulk.blocks.push(Some(StagedBlock {
            container,
            id: id.clone(),
            order,
            kind: kind.to_string(),
            attrs: BTreeMap::new(),
            text: None,
            text_len: 0,
        }));
        id
    }

    fn staged_mut(&mut self, id: &str) -> Option<&mut StagedBlock> {
        let bulk = &mut self.ids.bulk;
        let idx = *bulk.by_id.get(id)?;
        bulk.blocks.get_mut(idx)?.as_mut()
    }

    /// Whether `id` is a staged block living in `container`.
    pub(super) fn is_staged_in(&self, container: &ObjId, id: &str) -> bool {
        let bulk = &self.ids.bulk;
        bulk.by_id
            .get(id)
            .and_then(|&i| bulk.blocks.get(i))
            .and_then(Option::as_ref)
            .is_some_and(|b| &b.container == container)
    }

    /// Handle a text splice inside a bulk scope. Returns `false` when the caller must perform the
    /// splice on the document itself (after this has flushed whatever the splice depends on).
    pub(super) fn bulk_splice(
        &mut self,
        block_id: &str,
        idx: usize,
        del: isize,
        ins: &str,
    ) -> bool {
        if !self.bulk_active() {
            return false;
        }
        if self.ids.bulk.marked.contains(block_id) {
            self.flush_bulk();
            return false;
        }
        let Some(block) = self.staged_mut(block_id) else {
            return false;
        };
        if del != 0 || idx > block.text_len {
            // Deletions and out-of-range splices keep automerge's exact semantics.
            self.flush_bulk();
            return false;
        }
        let text = block.text.get_or_insert_with(String::new);
        if idx == block.text_len {
            // Importers append; only a mid-text insert needs the character-to-byte walk.
            text.push_str(ins);
        } else {
            let byte = text.char_indices().nth(idx).map_or(text.len(), |(b, _)| b);
            text.insert_str(byte, ins);
        }
        block.text_len = block.text_len.saturating_add(ins.chars().count());
        true
    }

    /// Stage a mark on a block of `container` (staged or already written).
    pub(super) fn stage_mark(
        &mut self,
        container: ObjId,
        block_id: &str,
        start: usize,
        end: usize,
        name: &str,
        value: ScalarValue,
    ) {
        if let Some(block) = self.staged_mut(block_id)
            && block.container == container
        {
            // Marking creates the text object, as the op-by-op path does.
            block.text.get_or_insert_with(String::new);
        }
        let bulk = &mut self.ids.bulk;
        bulk.marked.insert(block_id.to_string());
        bulk.marks.push(StagedMark {
            container,
            block: block_id.to_string(),
            start,
            end,
            name: name.to_string(),
            value,
        });
    }

    /// Set an attribute on a staged block of `container`, or hand `value` back if it isn't one.
    pub(super) fn stage_attr(
        &mut self,
        container: &ObjId,
        block_id: &str,
        key: &str,
        value: ScalarValue,
    ) -> Option<ScalarValue> {
        match self.staged_mut(block_id) {
            Some(block) if &block.container == container => {
                block.attrs.insert(key.to_string(), value);
                None
            }
            _ => Some(value),
        }
    }

    /// Set the kind of a staged block of `container`; `false` if it isn't one.
    pub(super) fn stage_kind(&mut self, container: &ObjId, block_id: &str, kind: &str) -> bool {
        match self.staged_mut(block_id) {
            Some(block) if &block.container == container => {
                kind.clone_into(&mut block.kind);
                true
            }
            _ => false,
        }
    }

    /// Drop a staged block of `container`; `false` if it isn't one.
    pub(super) fn unstage_block(&mut self, container: &ObjId, block_id: &str) -> bool {
        if !self.is_staged_in(container, block_id) {
            return false;
        }
        let bulk = &mut self.ids.bulk;
        if let Some(idx) = bulk.by_id.remove(block_id)
            && let Some(slot) = bulk.blocks.get_mut(idx)
        {
            *slot = None;
        }
        self.forget_order(container, block_id);
        true
    }

    /// The character length of a body block's text (0 if it has none), including staged blocks.
    pub fn block_text_len(&self, block_id: &str) -> usize {
        let bulk = &self.ids.bulk;
        if let Some(block) = bulk
            .by_id
            .get(block_id)
            .and_then(|&i| bulk.blocks.get(i))
            .and_then(Option::as_ref)
            .filter(|b| b.container == self.c.blocks)
        {
            return block.text_len;
        }
        super::block_text(self.doc, self.c, block_id).map_or(0, |t| t.chars().count())
    }

    /// Write every staged block and apply every staged mark.
    pub(super) fn flush_bulk(&mut self) {
        let bulk = &mut self.ids.bulk;
        if bulk.blocks.is_empty() && bulk.marks.is_empty() {
            return;
        }
        let mut blocks: Vec<StagedBlock> = std::mem::take(&mut bulk.blocks)
            .into_iter()
            .flatten()
            .collect();
        bulk.by_id = HashMap::new();
        let mut marks = std::mem::take(&mut bulk.marks);
        bulk.marked = HashSet::new();
        if !(bulk.may_commit || self.ids.commits_allowed) {
            write_as_ops(self.doc, blocks, marks);
            return;
        }
        // A host still owns a commit boundary after the import, and decides from its transaction
        // whether the action it brackets changed anything (an undo log records nothing for an empty
        // commit). So the last block stays in the host's transaction, as local ops, and the host
        // commits it; everything before it goes in as changes.
        let tail = if self.ids.commits_allowed {
            None
        } else {
            blocks.pop()
        };
        let tail_marks: Vec<StagedMark> = match &tail {
            Some(t) => {
                let (on_tail, rest) = marks
                    .into_iter()
                    .partition(|m| (&m.container, &m.block) == (&t.container, &t.id));
                marks = rest;
                on_tail
            }
            None => Vec::new(),
        };
        if !blocks.is_empty() && write_as_changes(self.doc, &blocks, &marks) {
            #[cfg(test)]
            CHANGE_WRITES.with(|c| c.set(c.get().saturating_add(1)));
            write_as_ops(self.doc, tail.into_iter().collect(), tail_marks);
            return;
        }
        blocks.extend(tail);
        marks.extend(tail_marks);
        write_as_ops(self.doc, blocks, marks);
    }
}

/// Write staged blocks as one batch object each, then apply the marks as local operations.
///
/// # Panics
/// Panics if a staged block's container no longer resolves — containers are resolved by this API
/// itself when a block is staged, see the note on `impl DocMut`.
#[allow(
    clippy::expect_used,
    reason = "the container ids were resolved by this API when the block was staged"
)]
fn write_as_ops(doc: &mut AutoCommit, blocks: Vec<StagedBlock>, marks: Vec<StagedMark>) {
    let encoding = doc.text_encoding();
    let marked: HashSet<(&ObjId, &str)> = marks
        .iter()
        .map(|m| (&m.container, m.block.as_str()))
        .collect();
    let mut written: HashMap<(ObjId, String), ObjId> = HashMap::new();
    for block in blocks {
        let key = (block.container.clone(), block.id.clone());
        let obj = doc
            .batch_create_object(&key.0, key.1.as_str(), &block.into_value(encoding), false)
            .expect("staged block container resolves");
        if marked.contains(&(&key.0, key.1.as_str())) {
            written.insert(key, obj);
        }
    }
    let mut texts: HashMap<(ObjId, String), Option<ObjId>> = HashMap::new();
    for m in marks {
        let key = (m.container.clone(), m.block.clone());
        let text = texts
            .entry(key.clone())
            .or_insert_with(|| {
                let obj = match written.get(&key) {
                    Some(obj) => obj.clone(),
                    None => block_obj(doc, &m.container, &m.block)?,
                };
                text_object(doc, &obj)
            })
            .clone();
        if let Some(text) = text {
            apply_mark_op(doc, &text, m);
        }
    }
}

/// A block's text object, created if missing (as marking a block without text does op by op).
fn text_object(doc: &mut AutoCommit, block: &ObjId) -> Option<ObjId> {
    match doc.get(block, BLOCK_TEXT) {
        Ok(Some((Value::Object(ObjType::Text), text))) => Some(text),
        _ => doc.put_object(block, BLOCK_TEXT, ObjType::Text).ok(),
    }
}

fn apply_mark_op(doc: &mut AutoCommit, text: &ObjId, m: StagedMark) {
    let mark = Mark::new(m.name, m.value, m.start, m.end);
    let _ = doc.mark(text, mark, ExpandMark::After);
}

/// Staged content bigger than this many ops is split over several changes, bounding the memory the
/// expanded ops take before they are encoded.
const OPS_PER_CHANGE: usize = 1 << 16;

fn legacy_obj(obj: &ObjId) -> legacy::ObjectId {
    match obj {
        ObjId::Root => legacy::ObjectId::Root,
        ObjId::Id(counter, actor, _) => legacy::ObjectId::Id(legacy::OpId(*counter, actor.clone())),
    }
}

/// Builds the changes that write staged blocks, numbering ops as automerge will.
struct ChangeWriter {
    actor: automerge::ActorId,
    seq: u64,
    /// Counter of the next op.
    next: u64,
    /// Counter of the first op of the change being built.
    start: std::num::NonZeroU64,
    deps: Vec<automerge::ChangeHash>,
    ops: Vec<legacy::Op>,
}

impl ChangeWriter {
    fn push(
        &mut self,
        obj: legacy::ObjectId,
        key: legacy::Key,
        action: legacy::OpType,
        insert: bool,
    ) -> legacy::OpId {
        let id = legacy::OpId(self.next, self.actor.clone());
        self.next = self.next.saturating_add(1);
        self.ops.push(legacy::Op {
            action,
            obj,
            key,
            pred: legacy::SortedVec::new(),
            insert,
        });
        id
    }

    fn put(&mut self, obj: &legacy::OpId, key: &str, action: legacy::OpType) -> legacy::OpId {
        self.push(
            legacy::ObjectId::Id(obj.clone()),
            legacy::Key::Map(key.into()),
            action,
            false,
        )
    }

    /// Close the change being built; `None` if it has no ops.
    fn seal(&mut self) -> Option<automerge::Change> {
        if self.ops.is_empty() {
            return None;
        }
        let change = automerge::Change::from(automerge::ExpandedChange {
            operations: std::mem::take(&mut self.ops),
            actor_id: self.actor.clone(),
            hash: None,
            seq: self.seq,
            start_op: self.start,
            time: 0,
            message: None,
            deps: std::mem::take(&mut self.deps),
            extra_bytes: Vec::new(),
            author: None,
        });
        self.deps = vec![change.hash()];
        self.seq = self.seq.saturating_add(1);
        // `next` only grows from `start`, which is at least one.
        self.start = std::num::NonZeroU64::new(self.next).unwrap_or(self.start);
        Some(change)
    }

    /// The ops that fill `block`, whose map `root` already created, and apply `marks` to its
    /// text: the tree [`StagedBlock::into_value`] describes, then per mark a begin anchor after the character
    /// before its start (not expanding) and an end anchor after the character before its end
    /// (expanding), which is where a local [`ExpandMark::After`] mark puts them.
    fn block(&mut self, block: &StagedBlock, root: &legacy::OpId, marks: &[&StagedMark]) {
        let attrs = self.put(root, BLOCK_ATTRS, legacy::OpType::Make(ObjType::Map));
        self.put(
            root,
            BLOCK_KIND,
            legacy::OpType::Put(block.kind.as_str().into()),
        );
        self.put(
            root,
            BLOCK_ORDER,
            legacy::OpType::Put(ScalarValue::Bytes(block.order.clone())),
        );
        for (key, value) in &block.attrs {
            self.put(&attrs, key, legacy::OpType::Put(value.clone()));
        }
        let Some(text) = &block.text else {
            return;
        };
        let text_id = self.put(root, BLOCK_TEXT, legacy::OpType::Make(ObjType::Text));
        let obj = legacy::ObjectId::Id(text_id);
        let first = self.next;
        let mut prev = legacy::ElementId::Head;
        for ch in text.chars() {
            let id = self.push(
                obj.clone(),
                legacy::Key::Seq(prev),
                legacy::OpType::Put(ch.into()),
                true,
            );
            prev = legacy::ElementId::Id(id);
        }
        for m in marks {
            let begin = self.anchor(first, m.start);
            let end = self.anchor(first, m.end);
            self.push(
                obj.clone(),
                legacy::Key::Seq(begin),
                legacy::OpType::MarkBegin(legacy::MarkData {
                    name: m.name.as_str().into(),
                    value: m.value.clone(),
                    expand: false,
                }),
                true,
            );
            self.push(
                obj.clone(),
                legacy::Key::Seq(end),
                legacy::OpType::MarkEnd(true),
                true,
            );
        }
    }

    /// The element a mark anchor at `pos` sits after, in a text whose character 0 has op counter
    /// `first`: the character before `pos`, or the head.
    fn anchor(&self, first: u64, pos: usize) -> legacy::ElementId {
        match pos.checked_sub(1).and_then(|p| u64::try_from(p).ok()) {
            Some(p) => {
                legacy::ElementId::Id(legacy::OpId(first.saturating_add(p), self.actor.clone()))
            }
            None => legacy::ElementId::Head,
        }
    }
}

/// Apply the change being built.
///
/// # Panics
/// Panics if automerge rejects it. The change continues this document's own history — built on
/// its heads, as its actor's next sequence number, over objects the previous changes created — so
/// a rejection means those invariants broke; carrying on would leave block maps without content.
#[allow(
    clippy::expect_used,
    reason = "a change built on this document's own heads and objects must apply; see above"
)]
fn apply_sealed(doc: &mut AutoCommit, writer: &mut ChangeWriter) {
    if let Some(change) = writer.seal() {
        apply_change(doc, change).expect("a change continuing this document's history applies");
    }
}

/// Apply one change, dropping the patch events automerge records for it when nothing will read
/// them.
///
/// Automerge 0.8 logs a patch event for every op a change integrates, into the document's patch
/// log, whether or not that log is active; an `AutoCommit` only drains an active one (armed by
/// `update_diff_cursor`). Left alone, an imported document keeps an event per character, roughly
/// ten times the memory of the document itself, for as long as it lives. Without a diff cursor the
/// log is inactive and its events are dead, so it is replaced by an empty one.
pub(crate) fn apply_change(
    doc: &mut AutoCommit,
    change: automerge::Change,
) -> Result<(), automerge::AutomergeError> {
    let applied = doc.apply_changes([change]);
    if doc.diff_cursor().is_empty() {
        doc.reset_diff_cursor();
    }
    applied
}

/// Commit, then write every staged block — with its text and the marks on it — as the ops of new
/// changes, applied in one go. Automerge integrates the ops a change makes on objects it has not
/// seen as one run at the end of its op set; written op by op, or even one batch object per block,
/// every block and every mark endpoint pays for its own op-set update, and marks pay the most.
///
/// Returns `false` (having changed nothing but the commit) when the changes can't be built or
/// applied, so the caller writes the ops locally instead. Marks a change can't state exactly — on
/// blocks created before the scope, zero-width, or out of range — are applied locally afterwards;
/// they share no text with a mark in the changes, or cover no character.
fn write_as_changes(doc: &mut AutoCommit, blocks: &[StagedBlock], marks: &[StagedMark]) -> bool {
    if doc.text_encoding() != TextEncoding::UnicodeCodePoint {
        return false;
    }
    let committed = doc.commit();
    let actor = doc.get_actor().clone();
    let last_seq = match committed {
        Some(hash) => match doc.get_change_meta_by_hash(&hash) {
            Some(meta) if meta.actor.as_ref() == &actor => meta.seq,
            _ => return false,
        },
        None => doc
            .get_changes_meta(&[])
            .iter()
            .filter(|meta| meta.actor.as_ref() == &actor)
            .map(|meta| meta.seq)
            .max()
            .unwrap_or(0),
    };
    let heads = doc.get_heads();
    let mut max_op = 0;
    for h in &heads {
        let Some(meta) = doc.get_change_meta_by_hash(h) else {
            return false;
        };
        max_op = max_op.max(meta.max_op);
    }
    let Some(start) = std::num::NonZeroU64::new(max_op.saturating_add(1)) else {
        return false;
    };

    let staged: HashMap<(&ObjId, &str), &StagedBlock> = blocks
        .iter()
        .map(|b| ((&b.container, b.id.as_str()), b))
        .collect();
    let mut in_change: HashMap<&str, Vec<&StagedMark>> = HashMap::new();
    let mut local: Vec<&StagedMark> = Vec::new();
    for m in marks {
        match staged.get(&(&m.container, m.block.as_str())) {
            // Non-empty and within the text.
            Some(b) if (m.start.saturating_add(1)..=b.text_len).contains(&m.end) => {
                in_change.entry(m.block.as_str()).or_default().push(m);
            }
            _ => local.push(m),
        }
    }

    let mut writer = ChangeWriter {
        actor,
        seq: last_seq.saturating_add(1),
        next: start.get(),
        start,
        deps: heads,
        ops: Vec::new(),
    };
    // Every block map first, in a change of their own: the maps are entries of containers that
    // already hold blocks, sorted by key, so they integrate as one run only if one change carries
    // them all. What follows lives in objects new to the document and appends in order.
    let roots: Vec<legacy::OpId> = blocks
        .iter()
        .map(|b| {
            writer.push(
                legacy_obj(&b.container),
                legacy::Key::Map(b.id.as_str().into()),
                legacy::OpType::Make(ObjType::Map),
                false,
            )
        })
        .collect();
    if let Some(change) = writer.seal()
        && apply_change(doc, change).is_err()
    {
        return false;
    }
    // Each further change is applied as soon as it is sealed: automerge expands a change's ops in
    // memory while integrating it, so one change for everything would hold the whole document
    // expanded at once.
    for (block, root) in blocks.iter().zip(&roots) {
        let on_block = in_change
            .get(block.id.as_str())
            .map_or(&[][..], Vec::as_slice);
        writer.block(block, root, on_block);
        if writer.ops.len() >= OPS_PER_CHANGE {
            apply_sealed(doc, &mut writer);
        }
    }
    apply_sealed(doc, &mut writer);
    let mut texts: HashMap<(&ObjId, &str), Option<ObjId>> = HashMap::new();
    for m in local {
        let text = texts
            .entry((&m.container, m.block.as_str()))
            .or_insert_with(|| {
                let obj = block_obj(doc, &m.container, &m.block)?;
                text_object(doc, &obj)
            })
            .clone();
        if let Some(text) = text {
            let mark = Mark::new(m.name.clone(), m.value.clone(), m.start, m.end);
            let _ = doc.mark(&text, mark, ExpandMark::After);
        }
    }
    true
}
