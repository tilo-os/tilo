//! Buffered block writes for whole-document imports (see [`DocMut::bulk`]).
//! Automerge applies every local operation to its op set on the spot, and each
//! costs a seek plus an update of every column and index; an inline mark rebuilds
//! the mark index around its anchor. Inside a bulk scope new blocks and marks live
//! in memory until the scope ends, then go in either as the ops of new changes
//! (integrate as one run, marks included) or, for a host-owned document with
//! history, as one batch object per block with marks applied as local operations
//! afterwards.

use super::{BLOCK_ATTRS, BLOCK_KIND, BLOCK_ORDER, BLOCK_TEXT, DocMut, ObjType, Value, block_obj};
use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ReadDoc, ScalarValue, TextEncoding, hydrate};
use std::collections::{BTreeMap, HashMap, HashSet};
use tilo_resource::batch::{self, ChangeWriter};

#[cfg(any(test, feature = "bulk-instrumentation"))]
thread_local! {
    /// Makes [`DocMut::bulk`] a plain call, so tests can build the op-by-op reference document.
    static OP_BY_OP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// How many flushes on this thread wrote their blocks as changes.
    static CHANGE_WRITES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(any(test, feature = "bulk-instrumentation"))]
pub fn change_writes() -> usize {
    CHANGE_WRITES.with(std::cell::Cell::get)
}

/// Run `f` with every bulk scope on this thread disabled (the op-by-op reference path).
#[cfg(any(test, feature = "bulk-instrumentation"))]
pub fn op_by_op<R>(f: impl FnOnce() -> R) -> R {
    OP_BY_OP.with(|c| c.set(true));
    let out = f();
    OP_BY_OP.with(|c| c.set(false));
    out
}

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

struct StagedMark {
    container: ObjId,
    block: String,
    start: usize,
    end: usize,
    name: String,
    value: ScalarValue,
}

/// The staging area of an open bulk scope. It rides on [`super::IdGen`], which already threads
/// through every [`DocMut`] over the same document.
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
    /// Run `f` with block creation buffered: blocks inserted inside the scope, with their kind,
    /// attributes and text, are written together when the outermost scope ends, and marks are
    /// applied after all text is in place. The resulting content is the same as without the scope.
    /// Operations that read or restructure existing blocks flush the buffer first; reads that
    /// bypass [`DocMut`] do not see staged blocks until the scope ends (use
    /// [`Self::block_text_len`] for the one read an importer needs). When bulk writes may commit,
    /// the scope ends by committing and writing the staged blocks and marks as new changes; that is
    /// decided at [`super::IdGen::allow_commits`] or for a document being created. Otherwise the
    /// last block stays in the host's open transaction, so the host's next commit carries the
    /// import and brackets it as one undoable action.
    pub fn bulk<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        #[cfg(any(test, feature = "bulk-instrumentation"))]
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

    pub(super) fn stage_block(
        &mut self,
        container: ObjId,
        after: Option<&str>,
        kind: &str,
    ) -> String {
        let id = self.ids.mint();
        let order = self.order_after_in(&container, after, &id);
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

    pub(super) fn is_staged_in(&self, container: &ObjId, id: &str) -> bool {
        let bulk = &self.ids.bulk;
        bulk.by_id
            .get(id)
            .and_then(|&i| bulk.blocks.get(i))
            .and_then(Option::as_ref)
            .is_some_and(|b| &b.container == container)
    }

    /// Handle a text splice inside a bulk scope; `false` when the caller must perform it on the
    /// document itself.
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

    pub(super) fn unstage_attr(&mut self, container: &ObjId, block_id: &str, key: &str) -> bool {
        match self.staged_mut(block_id) {
            Some(block) if &block.container == container => {
                block.attrs.remove(key);
                true
            }
            _ => false,
        }
    }

    pub(super) fn stage_kind(&mut self, container: &ObjId, block_id: &str, kind: &str) -> bool {
        match self.staged_mut(block_id) {
            Some(block) if &block.container == container => {
                kind.clone_into(&mut block.kind);
                true
            }
            _ => false,
        }
    }

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
        // whether the action it brackets changed anything, so the last block stays in the host's
        // transaction as local ops.
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
            #[cfg(any(test, feature = "bulk-instrumentation"))]
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
///
/// Panics if a staged block's container no longer resolves — containers are resolved by this API
/// when a block is staged.
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

/// Write one staged block, whose map `root` already created, with `marks` on
/// its text: the tree [`StagedBlock::into_value`] describes.
fn write_block(
    writer: &mut ChangeWriter,
    block: &StagedBlock,
    root: &batch::Created,
    marks: &[&StagedMark],
) {
    let attrs = writer.make(root, BLOCK_ATTRS, ObjType::Map);
    writer.put(root, BLOCK_KIND, block.kind.as_str().into());
    writer.put(root, BLOCK_ORDER, ScalarValue::Bytes(block.order.clone()));
    for (key, value) in &block.attrs {
        writer.put(&attrs, key, value.clone());
    }
    if let Some(text) = &block.text {
        let marks: Vec<batch::TextMark> = marks
            .iter()
            .map(|m| batch::TextMark {
                name: m.name.clone(),
                value: m.value.clone(),
                start: m.start,
                end: m.end,
            })
            .collect();
        writer.text(root, BLOCK_TEXT, text, &marks);
    }
}

#[allow(
    clippy::expect_used,
    reason = "a change built on this document's own heads and objects must apply; see above"
)]
fn apply_sealed(doc: &mut AutoCommit, writer: &mut ChangeWriter) {
    writer
        .apply(doc)
        .expect("a change continuing this document's history applies");
}

/// Commit, then write every staged block, with its text and marks, as the ops of new changes
/// applied in one go. Returns `false` (having changed nothing but the commit) when the changes
/// cannot be built or applied, so the caller writes the ops locally instead.
fn write_as_changes(doc: &mut AutoCommit, blocks: &[StagedBlock], marks: &[StagedMark]) -> bool {
    let Some(mut writer) = ChangeWriter::continuing(doc) else {
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
            Some(b) if (m.start.saturating_add(1)..=b.text_len).contains(&m.end) => {
                in_change.entry(m.block.as_str()).or_default().push(m);
            }
            _ => local.push(m),
        }
    }

    // Every block map first, in a change of their own: they are entries of containers that already
    // hold blocks, so they integrate as one run only if one change carries them all. What follows
    // lives in objects new to the document and appends in order.
    let roots: Vec<batch::Created> = blocks
        .iter()
        .map(|b| writer.make_in(&b.container, b.id.as_str(), ObjType::Map))
        .collect();
    if writer.apply(doc).is_err() {
        return false;
    }
    // Each further change is applied as soon as it is sealed: automerge expands a change's ops in
    // memory while integrating it.
    for (block, root) in blocks.iter().zip(&roots) {
        let on_block = in_change
            .get(block.id.as_str())
            .map_or(&[][..], Vec::as_slice);
        write_block(&mut writer, block, root, on_block);
        if writer.pending() >= batch::OPS_PER_CHANGE {
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
