//! Bring a document to the state an ingested file describes by diffing and
//! splicing it, instead of replacing it.
//!
//! A whole-file save — a `.md` edited in another editor, a `.docx` arriving
//! through the sync folder — parses into a fresh document that shares no history
//! with the one the store holds. Writing that document over the stored one,
//! deleting its containers and building them again, puts a new object under
//! every key: a peer's concurrent edit lands in an object nothing reaches any
//! more and disappears, block ids restart from zero, and every save re-adds the
//! whole document to the history.
//!
//! What a save actually says is "the document now reads like this". This module
//! writes that statement into the stored document as edits: a block that is in
//! both keeps its id, its text object and its marks; a changed paragraph is one
//! splice, not a delete and an insert. A peer's concurrent edit to another
//! paragraph — or to another part of the same paragraph — then merges as any
//! other edit does.
//!
//! What was written is checked against the ingested state before the caller
//! publishes it: a save writes what it says, or it is an error.

use crate::document::{DocError, MarkSpan, MarkValue};
use crate::model::{DocMut, Region};
use crate::schema::{ATTR_BLOB_BYTES, ATTR_BLOB_H, ATTR_BLOB_MIME, ATTR_BLOB_W, MARK_AUTHOR};
use crate::snapshot::{AttrVal, BlockView, DocSnapshot};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ScalarValue};
use std::collections::{BTreeMap, BTreeSet};
use tilo_resource::diff::{RunOp, text_runs};

/// What one save wrote into the document.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileStats {
    /// Blocks whose kind, text, attributes or marks changed.
    pub blocks_changed: usize,
    pub blocks_inserted: usize,
    pub blocks_removed: usize,
    /// Blocks that were in the document and the file alike, unchanged.
    pub blocks_unchanged: usize,
}

impl ReconcileStats {
    /// Whether the save left the document exactly as it was: nothing a peer
    /// could have seen changed, so there is nothing to publish.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks_changed == 0 && self.blocks_inserted == 0 && self.blocks_removed == 0
    }
}

/// Reconcile the document's root in `doc` into the state `source` describes.
/// Returns what changed, or `None` when `doc` holds no document to reconcile
/// into. The caller commits and publishes; nothing here reads the clock or an
/// author, so a replica that writes nothing adds nothing to the history.
///
/// # Errors
///
/// [`DocError::Invalid`] when the written state does not read back as `source`,
/// [`crate::model::IdGenError`]'s wrapped form when an id has to be minted and
/// this replica has used all of them, [`DocError::Automerge`] when a write fails.
/// Either way the open transaction holds partial work the caller must roll back.
pub fn reconcile_document(
    doc: &mut AutoCommit,
    source: &DocSnapshot,
) -> Result<Option<ReconcileStats>, DocError> {
    reconcile_under(doc, &automerge::ROOT, source)
}

/// [`reconcile_document`] for a document stored under `root` rather than at the
/// automerge root — the field of a larger record, as a file's own document is.
///
/// # Errors
///
/// As [`reconcile_document`].
pub fn reconcile_under(
    doc: &mut AutoCommit,
    root: &automerge::ObjId,
    source: &DocSnapshot,
) -> Result<Option<ReconcileStats>, DocError> {
    let Ok(c) = crate::model::open(doc, root) else {
        return Ok(None);
    };
    // An id minted here must not collide with one this document already holds
    // under a different object, so the generator is seeded past those.
    let ids = &mut crate::model::try_seeded_id_gen(doc, &c)?;
    let stats = reconcile_into(&mut DocMut { doc, c: &c, ids }, source)?;
    if doc.pending_ops() == 0 {
        return Ok(Some(stats));
    }
    Ok(Some(stats))
}

/// Reconcile every region, note, style, property and page setting of the
/// document `d` drives into the state `source` describes.
///
/// Notes are reconciled by position: the third footnote of a file is the third
/// footnote of the document, whatever ids either gave them. A note's id is the
/// document's name for it and the name `note-ref` marks already point at, so it
/// never comes from the file and no save can re-mint one.
///
/// # Errors
///
/// As [`reconcile_document`].
pub fn reconcile_into(
    d: &mut DocMut<'_>,
    source: &DocSnapshot,
) -> Result<ReconcileStats, DocError> {
    let mut w = Writer {
        d,
        stats: ReconcileStats::default(),
    };
    w.notes(source);
    w.blocks(&Place::Region(Region::Body), &source.blocks);
    w.blocks(&Place::Region(Region::Header), &source.header);
    w.blocks(&Place::Region(Region::Footer), &source.footer);
    let stats = w.stats;
    let d = w.d;
    styles(d, &source.styles);
    for (name, value) in &source.properties {
        if crate::model::doc_prop(d.doc, d.c, name).as_deref() != Some(value.as_str()) {
            d.set_doc_prop(name, value);
        }
    }
    let (columns, gap) = crate::model::columns(d.doc, d.c);
    if columns != source.columns || gap != source.column_gap.map(f64::from) {
        d.set_columns(source.columns, source.column_gap.map(f64::from));
    }
    if crate::model::language(d.doc, d.c) != source.language {
        d.set_language(source.language.as_deref().unwrap_or_default());
    }
    Ok(stats)
}

/// Whether `doc`'s root reads as `source` does: every block by position, kind,
/// text, marks and attributes, and the document-level settings beside them. Ids
/// are not compared — they belong to the document, not to the state it is in.
#[must_use]
pub fn reads_as(doc: &AutoCommit, root: &automerge::ObjId, source: &DocSnapshot) -> bool {
    let Ok(c) = crate::model::open(doc, root) else {
        return false;
    };
    let written = crate::model::DocRef { doc, c: &c }.materialize();
    same_state(&written, source)
}

// ---- document-level settings -------------------------------------------

/// Write every named style the file has and drop the ones it does not.
fn styles(d: &mut DocMut<'_>, wanted: &BTreeMap<String, BTreeMap<String, AttrVal>>) {
    let have = crate::model::all_styles(d.doc, d.c);
    for (id, props) in wanted {
        if have.get(id) != Some(props) {
            d.set_style(id, props);
        }
    }
    for id in have.keys() {
        if !wanted.contains_key(id) {
            d.remove_style(id);
        }
    }
}

// ---- the state, not the storage ----------------------------------------

/// Whether two documents read the same: the same block sequences in every
/// region and note, and the same settings beside them.
fn same_state(written: &DocSnapshot, source: &DocSnapshot) -> bool {
    same_blocks(&written.blocks, &source.blocks)
        && same_blocks(&written.header, &source.header)
        && same_blocks(&written.footer, &source.footer)
        && written.styles == source.styles
        && written.properties == source.properties
        && written.columns == source.columns
        && written.column_gap == source.column_gap
        && written.language == source.language
        && written.notes.len() == source.notes.len()
        && written
            .notes
            .iter()
            .zip(&source.notes)
            .all(|(a, b)| a.kind == b.kind && same_blocks(&a.blocks, &b.blocks))
}

fn same_blocks(written: &[BlockView], source: &[BlockView]) -> bool {
    written.len() == source.len() && written.iter().zip(source).all(|(a, b)| same_block(a, b))
}

/// Whether a live block reads like the one a file describes.
fn same_block(have: &BlockView, wanted: &BlockView) -> bool {
    have.kind == wanted.kind
        && have.text == wanted.text
        && visible_attrs(&have.attrs) == visible_attrs(&wanted.attrs)
        && same_marks(&have.marks, &wanted.marks)
        && same_blocks(&have.children, &wanted.children)
}

/// The spans a document shows, in a comparable form.
///
/// Authorship is left out: who typed a run is a fact about this document's
/// people, and no file it is written to carries one.
fn same_marks(written: &[MarkSpan], source: &[MarkSpan]) -> bool {
    let shown = |spans: &[MarkSpan]| -> BTreeSet<_> {
        spans
            .iter()
            .filter(|span| span.name != MARK_AUTHOR && span.start < span.end)
            .map(|span| {
                (
                    span.start,
                    span.end,
                    span.name.clone(),
                    span_value(&span.value),
                )
            })
            .collect()
    };
    shown(written) == shown(source)
}

/// The attributes a state carries, without the content descriptors the document
/// projects onto image blocks: the blobs map owns those, and a file carries the
/// blob hash itself.
fn visible_attrs(attrs: &BTreeMap<String, AttrVal>) -> BTreeMap<String, AttrVal> {
    let hidden = [ATTR_BLOB_BYTES, ATTR_BLOB_W, ATTR_BLOB_H, ATTR_BLOB_MIME];
    attrs
        .iter()
        .filter(|(key, _)| !hidden.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// A mark value in a comparable form, so two documents' spans can be set
/// against each other.
fn span_value(value: &MarkValue) -> (u8, i64, String) {
    match value {
        MarkValue::Bool(b) => (0, i64::from(*b), String::new()),
        MarkValue::Int(n) => (1, *n, String::new()),
        MarkValue::Str(s) => (2, 0, s.clone()),
    }
}

fn scalar_of(value: &MarkValue) -> ScalarValue {
    match value {
        MarkValue::Bool(b) => ScalarValue::Boolean(*b),
        MarkValue::Int(n) => ScalarValue::Int(*n),
        MarkValue::Str(s) => ScalarValue::Str(s.as_str().into()),
    }
}

fn attr_value(value: &AttrVal) -> ScalarValue {
    match value {
        AttrVal::Int(n) => ScalarValue::Int(*n),
        AttrVal::Bool(b) => ScalarValue::Boolean(*b),
        AttrVal::Str(s) => ScalarValue::Str(s.as_str().into()),
        AttrVal::Float(f) => ScalarValue::F64(*f),
    }
}

// ---- aligning a file against a document --------------------------------

/// Align the blocks a document holds against the ones a file shows, as
/// `(document index, file index)` pairs of blocks that are the same block.
///
/// [`align`] pairs ids, and a save carries none: parsing a file mints fresh
/// ones, so matching by id would pair nothing and rewrite everything. Two blocks
/// pair here when they are equal, in order — the anchors an edit leaves alone —
/// and everything between two anchors is the edit.
fn anchors(live: &[BlockView], want: &[&BlockView]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut from = 0usize;
    for (live_index, block) in live.iter().enumerate() {
        let Some(offset) = want
            .get(from..)
            .and_then(|rest| rest.iter().position(|view| same_block(block, view)))
        else {
            continue;
        };
        let want_index = from.saturating_add(offset);
        pairs.push((live_index, want_index));
        from = want_index.saturating_add(1);
    }
    pairs
}

/// The id of the block before index `at`, which an insertion goes after.
fn previous(live: &[BlockView], at: usize) -> Option<&str> {
    live.get(at.checked_sub(1)?).map(|view| view.id.as_str())
}

/// Where a block sequence lives: a page region, or one note's body.
///
/// The document's model names page regions ([`Region`]); a note's body is a
/// block sequence too, but it is reached through the note rather than through a
/// region, and reconciling one needs the same diff. This names either.
#[derive(Clone, PartialEq, Eq)]
enum Place {
    Region(Region),
    Note(String),
}

impl Place {
    /// The blocks here as they read now, in document order.
    fn blocks(&self, d: &DocMut<'_>) -> Vec<BlockView> {
        match self {
            Self::Region(region) => crate::model::materialize_region(d.doc, d.c, *region),
            Self::Note(id) => crate::model::materialize_note_blocks(d.doc, d.c, id),
        }
    }

    /// Insert an empty block of `kind` after `after` (before `next` when given).
    fn insert(
        &self,
        d: &mut DocMut<'_>,
        after: Option<&str>,
        next: Option<&str>,
        kind: &str,
    ) -> Option<String> {
        match self {
            Self::Region(region) => Some(match (after, next) {
                (Some(prev), Some(next)) => {
                    d.insert_block_before_in(*region, prev, Some(next), kind)
                }
                (after, _) => d.insert_block_in(*region, after, kind),
            }),
            Self::Note(id) => d.insert_note_block(id, after, kind),
        }
    }

    fn remove(&self, d: &mut DocMut<'_>, id: &str) {
        match self {
            Self::Region(region) => d.remove_block_in(*region, id),
            Self::Note(note) => d.remove_note_block(note, id),
        }
    }

    fn set_kind(&self, d: &mut DocMut<'_>, id: &str, kind: &str) {
        match self {
            Self::Region(region) => d.set_kind_in(*region, id, kind),
            Self::Note(note) => d.set_note_kind(note, id, kind),
        }
    }

    fn splice(&self, d: &mut DocMut<'_>, id: &str, idx: usize, del: isize, ins: &str) {
        match self {
            Self::Region(region) => d.splice_text_in(*region, id, idx, del, ins),
            Self::Note(note) => d.splice_note_text(note, id, idx, del, ins),
        }
    }

    fn set_attr(&self, d: &mut DocMut<'_>, id: &str, key: &str, value: ScalarValue) {
        match self {
            Self::Region(region) => d.set_attr_in(*region, id, key, value),
            Self::Note(note) => d.set_note_attr(note, id, key, value),
        }
    }

    fn remove_attr(&self, d: &mut DocMut<'_>, id: &str, key: &str) {
        match self {
            Self::Region(region) => d.remove_attr_in(*region, id, key),
            Self::Note(note) => d.remove_note_attr(note, id, key),
        }
    }

    fn add_mark(
        &self,
        d: &mut DocMut<'_>,
        id: &str,
        start: usize,
        end: usize,
        name: &str,
        value: ScalarValue,
    ) {
        match self {
            Self::Region(region) => d.add_mark_in(*region, id, start, end, name, value),
            Self::Note(note) => d.add_note_mark(note, id, start, end, name, value),
        }
    }

    fn remove_mark(&self, d: &mut DocMut<'_>, id: &str, start: usize, end: usize, name: &str) {
        match self {
            Self::Region(region) => d.remove_mark_in(*region, id, start, end, name),
            Self::Note(note) => d.remove_note_mark(note, id, start, end, name),
        }
    }
}

struct Writer<'a, 'b> {
    d: &'a mut DocMut<'b>,
    stats: ReconcileStats,
}

impl Writer<'_, '_> {
    // ---- block sequences -------------------------------------------------

    /// Write one block sequence.
    ///
    /// Blocks the document and the file hold unchanged keep their ids; what
    /// differs between two of them is an edit of that gap, not a rewrite. A file
    /// says nothing about ids, so nothing here pairs them: see [`anchors`].
    fn blocks(&mut self, place: &Place, wanted: &[BlockView]) {
        let live = place.blocks(self.d);
        let want: Vec<&BlockView> = wanted.iter().collect();
        let anchors = anchors(&live, &want);
        let (mut live_at, mut want_at) = (0usize, 0usize);
        for &(live_index, want_index) in &anchors {
            let gap = (live_at..live_index).collect::<Vec<_>>();
            let added = (want_at..want_index).collect::<Vec<_>>();
            self.gap(
                place,
                (&live, &want),
                (&gap, &added),
                (previous(&live, live_at), Some(live_index)),
            );
            let Some(have) = live.get(live_index) else {
                continue;
            };
            if let Some(view) = want.get(want_index) {
                self.block(place, have, view);
            }
            live_at = live_index.saturating_add(1);
            want_at = want_index.saturating_add(1);
        }
        let gap = (live_at..live.len()).collect::<Vec<_>>();
        let added = (want_at..want.len()).collect::<Vec<_>>();
        self.gap(
            place,
            (&live, &want),
            (&gap, &added),
            (live.last().map(|view| view.id.as_str()), None),
        );
    }

    /// Write what sits between two anchoring blocks: the document's blocks the
    /// file does not have are removed, the file's blocks the document does not
    /// have are inserted, and a removal beside an insertion is one block edited
    /// in place, so it keeps its id and its text object.
    fn gap(
        &mut self,
        place: &Place,
        blocks: (&[BlockView], &[&BlockView]),
        removed_and_added: (&[usize], &[usize]),
        around: (Option<&str>, Option<usize>),
    ) {
        let (live, want) = blocks;
        let (removed, added) = removed_and_added;
        let paired = removed.len().min(added.len());
        for (have, view) in removed.iter().zip(added) {
            let (Some(have), Some(view)) = (live.get(*have), want.get(*view)) else {
                continue;
            };
            self.block(place, have, view);
        }
        for index in removed.get(paired..).unwrap_or(&[]) {
            if let Some(have) = live.get(*index) {
                place.remove(self.d, &have.id);
                self.stats.blocks_removed = self.stats.blocks_removed.saturating_add(1);
            }
        }
        // An inserted block starts empty, so its text is one splice of the
        // whole run and its attributes one write each; reading it back to
        // compare would walk the sequence it was just added to.
        let next = around
            .1
            .and_then(|index| live.get(index))
            .map(|view| view.id.clone());
        let mut at: Option<String> = around.0.map(str::to_string);
        for index in added.get(paired..).unwrap_or(&[]) {
            let Some(view) = want.get(*index) else {
                continue;
            };
            let Some(id) = place.insert(self.d, at.as_deref(), next.as_deref(), &view.kind) else {
                continue;
            };
            self.stats.blocks_inserted = self.stats.blocks_inserted.saturating_add(1);
            self.write(place, &id, view);
            at = Some(id);
        }
    }

    /// Reconcile the live block that reads as `current` onto the one the file
    /// describes: kind, text, attributes and marks. A block that already reads
    /// like the file costs one comparison and writes nothing.
    fn block(&mut self, place: &Place, current: &BlockView, view: &BlockView) {
        if same_block(current, view) {
            self.stats.blocks_unchanged = self.stats.blocks_unchanged.saturating_add(1);
            return;
        }
        self.fill(place, current, view);
        self.stats.blocks_changed = self.stats.blocks_changed.saturating_add(1);
    }

    /// Write `view`'s content onto an empty block: a fresh insert, or one whose
    /// every part is about to be replaced. It reads nothing back, so it costs
    /// one write per part.
    fn write(&mut self, place: &Place, id: &str, view: &BlockView) {
        if !view.text.is_empty() {
            place.splice(self.d, id, 0, 0, &view.text);
        }
        for (key, value) in &visible_attrs(&view.attrs) {
            place.set_attr(self.d, id, key, attr_value(value));
        }
        for span in &view.marks {
            if span.name == MARK_AUTHOR || span.start >= span.end {
                continue;
            }
            place.add_mark(
                self.d,
                id,
                span.start,
                span.end,
                &span.name,
                scalar_of(&span.value),
            );
        }
        // A codec import produces no nesting today — a table cell is a flat
        // block carrying `row`/`col` — so children are compared, never written.
        debug_assert!(view.children.is_empty(), "a codec import nests no blocks");
    }

    /// Write `view`'s content onto the block that reads as `current`.
    fn fill(&mut self, place: &Place, current: &BlockView, view: &BlockView) {
        let id = current.id.as_str();
        if current.kind != view.kind {
            place.set_kind(self.d, id, &view.kind);
        }
        self.text(place, id, &current.text, &view.text);
        self.attrs(place, id, &current.attrs, &view.attrs);
        self.marks(place, id, &current.marks, &view.marks);
        // A codec import produces no nesting today — a table cell is a flat
        // block carrying `row`/`col` — so children are compared, never written.
        debug_assert!(view.children.is_empty(), "a codec import nests no blocks");
    }

    /// Splice the block's text from what it holds to what the file says: one
    /// operation per run that differs, so a peer's concurrent edit beside it
    /// stays.
    fn text(&mut self, place: &Place, id: &str, have: &str, wanted: &str) {
        if have == wanted {
            return;
        }
        let mut index = 0usize;
        for run in text_runs(have, wanted) {
            let chars = run.text.chars().count();
            match run.op {
                RunOp::Equal => index = index.saturating_add(chars),
                RunOp::Delete => {
                    let removed = isize::try_from(chars).unwrap_or(isize::MAX);
                    place.splice(self.d, id, index, removed, "");
                }
                RunOp::Insert => {
                    place.splice(self.d, id, index, 0, &run.text);
                    index = index.saturating_add(chars);
                }
            }
        }
    }

    /// Set the attributes the file has and drop the ones it does not. An absent
    /// attribute is the attribute's default, so a removed one is deleted rather
    /// than left behind as a stale value.
    fn attrs(
        &mut self,
        place: &Place,
        id: &str,
        have: &BTreeMap<String, AttrVal>,
        wanted: &BTreeMap<String, AttrVal>,
    ) {
        let have = visible_attrs(have);
        let wanted = visible_attrs(wanted);
        for (key, value) in &wanted {
            if have.get(key) != Some(value) {
                place.set_attr(self.d, id, key, attr_value(value));
            }
        }
        for key in have.keys() {
            if !wanted.contains_key(key) {
                place.remove_attr(self.d, id, key);
            }
        }
    }

    /// Cover exactly the spans the file marks. The text already reads as the
    /// file's, so a span's offsets mean the same on both sides.
    ///
    /// An author run is this document's record of who typed what: it stays on
    /// text that is still there, and no file can assert one.
    fn marks(&mut self, place: &Place, id: &str, have: &[MarkSpan], wanted: &[MarkSpan]) {
        let key = |span: &MarkSpan| {
            (
                span.start,
                span.end,
                span.name.clone(),
                span_value(&span.value),
            )
        };
        let have_at: BTreeSet<_> = have
            .iter()
            .filter(|span| span.name != MARK_AUTHOR && span.start < span.end)
            .map(key)
            .collect();
        let want_at: BTreeSet<_> = wanted
            .iter()
            .filter(|span| span.name != MARK_AUTHOR && span.start < span.end)
            .map(key)
            .collect();
        for span in have {
            if want_at.contains(&key(span)) {
                continue;
            }
            place.remove_mark(self.d, id, span.start, span.end, &span.name);
        }
        for span in wanted {
            if span.start >= span.end || span.name == MARK_AUTHOR || have_at.contains(&key(span)) {
                continue;
            }
            place.add_mark(
                self.d,
                id,
                span.start,
                span.end,
                &span.name,
                scalar_of(&span.value),
            );
        }
    }

    /// Bring each note's blocks to what the file shows, by position. A note's id
    /// is the document's name for it and what its `note-ref` marks point at, so
    /// matching by position is what keeps a reference pointing at its note.
    fn notes(&mut self, source: &DocSnapshot) {
        let live = crate::model::note_ids(self.d.doc, self.d.c);
        for (position, note) in source.notes.iter().enumerate() {
            let Some(id) = live.get(position) else {
                continue;
            };
            let place = Place::Note(id.clone());
            if same_blocks(&place.blocks(self.d), &note.blocks) {
                continue;
            }
            self.blocks(&place, &note.blocks);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;

    /// Build a document holding `text` as one paragraph per line, in order, and
    /// return it with what it reads as.
    fn document(text: &[&str]) -> (Document, DocSnapshot) {
        let mut doc = Document::new();
        for line in text {
            let id = doc.insert_block(last(&doc).as_deref(), "paragraph");
            doc.splice_text(&id, 0, 0, line);
        }
        let snapshot = doc.as_ref().materialize();
        (doc, snapshot)
    }

    /// The last block of a document, which the next one is inserted after.
    fn last(doc: &Document) -> Option<String> {
        ids(doc).pop()
    }

    fn ids(doc: &Document) -> Vec<String> {
        doc.as_ref()
            .materialize()
            .blocks
            .into_iter()
            .map(|block| block.id)
            .collect()
    }

    #[test]
    fn an_unchanged_save_writes_nothing() {
        let (mut doc, snapshot) = document(&["one", "two"]);
        let before = ids(&doc);
        let stats = reconcile_document(&mut doc.doc, &snapshot)
            .expect("reconcile")
            .expect("a document to reconcile into");
        doc.doc.commit();
        assert!(stats.is_empty(), "an unchanged save wrote {stats:?}");
        assert_eq!(before, ids(&doc));
        assert!(reads_as(&doc.doc, &automerge::ROOT, &snapshot));
    }

    #[test]
    fn a_changed_paragraph_keeps_its_block_id() {
        let (mut doc, snapshot) = document(&["one", "two"]);
        let before = ids(&doc);
        let (_other, wanted) = document(&["one", "two and a half"]);
        let stats = reconcile_document(&mut doc.doc, &wanted)
            .expect("reconcile")
            .expect("a document to reconcile into");
        doc.doc.commit();
        assert_eq!(before, ids(&doc), "an edit must not re-mint block ids");
        assert!(stats.blocks_changed > 0, "the edit was not written");
        assert!(reads_as(&doc.doc, &automerge::ROOT, &wanted));
        assert!(!reads_as(&doc.doc, &automerge::ROOT, &snapshot));
    }

    #[test]
    fn a_save_merges_a_peer_edit_to_another_paragraph() {
        // Two replicas of one document; the peer edits the second paragraph
        // while this one is about to save a file that only touches the first.
        let (mut mine, _) = document(&["first", "second"]);
        let mut theirs = Document::load(&mine.doc.save()).expect("load");
        let peer_block = ids(&theirs).get(1).expect("a second paragraph").clone();
        theirs.splice_text(&peer_block, 6, 0, " paragraph");

        let (_file, wanted) = document(&["first, edited", "second"]);
        reconcile_document(&mut mine.doc, &wanted)
            .expect("reconcile")
            .expect("a document to reconcile into");
        mine.doc.commit();
        mine.merge(&mut theirs);

        let text = |doc: &Document, id: &str| {
            doc.as_ref()
                .materialize()
                .blocks
                .into_iter()
                .find(|block| block.id == id)
                .map(|block| block.text)
                .unwrap_or_default()
        };
        assert_eq!(text(&mine, &peer_block).as_str(), "second paragraph");
        let first = ids(&mine).first().expect("a first paragraph").clone();
        assert_eq!(text(&mine, &first).as_str(), "first, edited");
    }
}
