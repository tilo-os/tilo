//! `Document`: an owned handle over an automerge `AutoCommit` holding the
//! canonical rich-text model. Thin wrapper that delegates to [`crate::model`];
//! the borrowed [`crate::model::DocMut`] view drives the same operations over a
//! host's doc (e.g. `ui::State.doc`).

use crate::model::{Containers, DocMut, DocRef, IdGen};
use automerge::{AutoCommit, ObjId, ROOT};

#[derive(thiserror::Error, Debug)]
pub enum DocError {
    #[error("automerge error: {0}")]
    Automerge(#[from] automerge::AutomergeError),
    #[error("load error: {0}")]
    Load(String),
    #[error("invalid document: {0}")]
    Invalid(String),
}

/// The value carried by a mark. Boolean marks (`strong`/`em`/`underline`) carry
/// `Bool(true)`; valued marks like `fontSize`/`fontFamily`/`link`/`color` carry
/// `Int`/`Str`.
///
/// `Ord` is derived so materialization can canonicalise the mark order *including
/// the value* — two replicas that merged concurrent valued marks in different
/// internal orders then sort to a byte-identical span list (convergence).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum MarkValue {
    Bool(bool),
    Int(i64),
    Str(String),
}

impl MarkValue {
    pub fn as_int(&self) -> Option<i64> {
        match self {
            MarkValue::Int(n) => Some(*n),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            MarkValue::Str(s) => Some(s),
            _ => None,
        }
    }
}

impl From<&automerge::ScalarValue> for MarkValue {
    fn from(v: &automerge::ScalarValue) -> Self {
        use automerge::ScalarValue;
        match v {
            ScalarValue::Boolean(b) => MarkValue::Bool(*b),
            ScalarValue::Int(i) => MarkValue::Int(*i),
            ScalarValue::Uint(u) => MarkValue::Int(*u as i64),
            ScalarValue::Str(s) => MarkValue::Str(s.to_string()),
            _ => MarkValue::Bool(true),
        }
    }
}

/// One inline formatting span as seen from the read side.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MarkSpan {
    pub start: usize,
    pub end: usize,
    pub name: String,
    pub value: MarkValue,
}

pub struct Document {
    pub(crate) doc: AutoCommit,
    #[allow(dead_code)]
    pub(crate) root: ObjId,
    pub(crate) c: Containers,
    ids: IdGen,
}

impl Document {
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        let c = crate::model::init(&mut doc, &ROOT);
        let ids = IdGen::new(doc.get_actor().to_hex_string());
        doc.commit();
        Self { doc, root: ROOT, c, ids }
    }

    pub fn load(bytes: &[u8]) -> Result<Self, DocError> {
        let doc = AutoCommit::load(bytes).map_err(|e| DocError::Load(e.to_string()))?;
        let c = crate::model::open(&doc, &ROOT)?;
        let ids = crate::model::seeded_id_gen(&doc, &c);
        Ok(Self { doc, root: ROOT, c, ids })
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    pub fn actor_hex(&self) -> String {
        self.doc.get_actor().to_hex_string()
    }

    /// Borrowed mutable view (for codecs / shared editing paths).
    pub fn as_mut(&mut self) -> DocMut<'_> {
        DocMut { doc: &mut self.doc, c: &self.c, ids: &mut self.ids }
    }

    /// Borrowed read-only view (for codec export).
    pub fn as_ref(&self) -> DocRef<'_> {
        DocRef { doc: &self.doc, c: &self.c }
    }

    // ---- delegating convenience API ---------------------------------------

    // The owned `Document` commits per op so its change history (the timeline)
    // stays fine-grained. (Borrowed `DocMut` does not — see model.rs.)
    pub fn insert_block(&mut self, after: Option<&str>, kind: &str) -> String {
        let id = self.as_mut().insert_block(after, kind);
        self.doc.commit();
        id
    }

    pub fn remove_block(&mut self, id: &str) {
        self.as_mut().remove_block(id);
        self.doc.commit();
    }

    pub fn move_block(&mut self, id: &str, after: Option<&str>) {
        self.as_mut().move_block(id, after);
        self.doc.commit();
    }

    pub fn splice_text(&mut self, block_id: &str, char_idx: usize, del: isize, ins: &str) {
        self.as_mut().splice_text(block_id, char_idx, del, ins);
        self.doc.commit();
    }

    pub fn add_mark(&mut self, block_id: &str, start: usize, end: usize, name: &str, value: impl Into<automerge::ScalarValue>) {
        self.as_mut().add_mark(block_id, start, end, name, value);
        self.doc.commit();
    }

    /// Store bytes in the content-addressed blob store; returns the blake3 hex
    /// hash (used as an `image` block's `blob` attr).
    pub fn put_blob(&mut self, bytes: &[u8]) -> String {
        let hash = self.as_mut().put_blob(bytes);
        self.doc.commit();
        hash
    }

    /// Fetch a content-addressed blob by its blake3 hex hash.
    pub fn blob(&self, hash: &str) -> Option<Vec<u8>> {
        crate::model::blob(&self.doc, &self.c, hash)
    }

    /// Set a document property (`title`/`author`/`subject`/`keywords`/`created`/
    /// `modified`/`generator`). Round-trips through `.odt`/`.docx`.
    pub fn set_doc_prop(&mut self, name: &str, value: &str) {
        self.as_mut().set_doc_prop(name, value);
        self.doc.commit();
    }

    /// Read a single document property, or `None` if unset.
    pub fn doc_prop(&self, name: &str) -> Option<String> {
        crate::model::doc_prop(&self.doc, &self.c, name)
    }

    /// All document properties (`name → value`).
    pub fn doc_props(&self) -> std::collections::BTreeMap<String, String> {
        crate::model::doc_props(&self.doc, &self.c)
    }

    /// Set the document-wide multi-column layout: `count` newspaper-style columns
    /// for the body, with an optional `gap` (px) between them. `count <= 1` clears
    /// the setting (single column). Round-trips through `.odt`/`.docx`.
    pub fn set_columns(&mut self, count: u32, gap: Option<f64>) {
        self.as_mut().set_columns(count, gap);
        self.doc.commit();
    }

    /// The document-wide multi-column layout: `(column_count, optional_gap_px)`.
    /// Defaults to `(1, None)` (single column) for documents with no column setting.
    pub fn columns(&self) -> (u32, Option<f64>) {
        crate::model::columns(&self.doc, &self.c)
    }

    pub fn blocks_in_order(&self) -> Vec<String> {
        crate::model::blocks_in_order(&self.doc, &self.c)
    }

    pub fn block_kind(&self, id: &str) -> Option<String> {
        crate::model::block_kind(&self.doc, &self.c, id)
    }

    pub fn block_text(&self, block_id: &str) -> Option<String> {
        crate::model::block_text(&self.doc, &self.c, block_id)
    }

    pub fn marks(&self, block_id: &str) -> Vec<MarkSpan> {
        crate::model::marks(&self.doc, &self.c, block_id)
    }

    pub fn materialize(&self) -> crate::snapshot::DocSnapshot {
        crate::model::materialize(&self.doc, &self.c)
    }

    // ---- page header / footer regions -------------------------------------

    /// The ordered block ids of the page header (empty if no header is set).
    pub fn header_blocks_in_order(&self) -> Vec<String> {
        crate::model::region_blocks_in_order(&self.doc, &self.c, crate::model::Region::Header)
    }

    /// The ordered block ids of the page footer (empty if no footer is set).
    pub fn footer_blocks_in_order(&self) -> Vec<String> {
        crate::model::region_blocks_in_order(&self.doc, &self.c, crate::model::Region::Footer)
    }

    /// Materialize the page header's blocks (empty if none).
    pub fn header(&self) -> Vec<crate::snapshot::BlockView> {
        crate::model::materialize_region(&self.doc, &self.c, crate::model::Region::Header)
    }

    /// Materialize the page footer's blocks (empty if none).
    pub fn footer(&self) -> Vec<crate::snapshot::BlockView> {
        crate::model::materialize_region(&self.doc, &self.c, crate::model::Region::Footer)
    }

    /// Insert a block into the page header (created lazily); returns its id.
    pub fn insert_header_block(&mut self, after: Option<&str>, kind: &str) -> String {
        let id = self.as_mut().insert_block_in(crate::model::Region::Header, after, kind);
        self.doc.commit();
        id
    }

    /// Insert a block into the page footer (created lazily); returns its id.
    pub fn insert_footer_block(&mut self, after: Option<&str>, kind: &str) -> String {
        let id = self.as_mut().insert_block_in(crate::model::Region::Footer, after, kind);
        self.doc.commit();
        id
    }

    /// Splice text into a header block.
    pub fn splice_header_text(&mut self, block_id: &str, char_idx: usize, del: isize, ins: &str) {
        self.as_mut().splice_text_in(crate::model::Region::Header, block_id, char_idx, del, ins);
        self.doc.commit();
    }

    /// Splice text into a footer block.
    pub fn splice_footer_text(&mut self, block_id: &str, char_idx: usize, del: isize, ins: &str) {
        self.as_mut().splice_text_in(crate::model::Region::Footer, block_id, char_idx, del, ins);
        self.doc.commit();
    }

    /// Insert a page-number field at `char_idx` in a header block.
    pub fn insert_header_page_number(&mut self, block_id: &str, char_idx: usize) {
        self.as_mut().insert_page_number_field(crate::model::Region::Header, block_id, char_idx);
        self.doc.commit();
    }

    /// Insert a page-number field at `char_idx` in a footer block.
    pub fn insert_footer_page_number(&mut self, block_id: &str, char_idx: usize) {
        self.as_mut().insert_page_number_field(crate::model::Region::Footer, block_id, char_idx);
        self.doc.commit();
    }

    // ---- footnotes / endnotes ---------------------------------------------

    /// The note ids present in the document (sorted, canonical order).
    pub fn note_ids(&self) -> Vec<String> {
        crate::model::note_ids(&self.doc, &self.c)
    }

    /// A note's kind (`Footnote`/`Endnote`), or `None` if the id is unknown.
    pub fn note_kind(&self, id: &str) -> Option<crate::model::NoteKind> {
        crate::model::note_kind(&self.doc, &self.c, id)
    }

    /// Materialize a note's content blocks (empty if the id is unknown).
    pub fn note_blocks(&self, id: &str) -> Vec<crate::snapshot::BlockView> {
        crate::model::materialize_note_blocks(&self.doc, &self.c, id)
    }

    /// The ordered block ids of a note's content (empty if absent).
    pub fn note_blocks_in_order(&self, id: &str) -> Vec<String> {
        crate::model::note_blocks_in_order(&self.doc, &self.c, id)
    }

    /// Materialize every note into a [`crate::snapshot::NoteView`].
    pub fn notes(&self) -> Vec<crate::snapshot::NoteView> {
        crate::model::materialize_notes(&self.doc, &self.c)
    }

    /// Create a footnote, place its reference marker at `char_idx` in `block_id`,
    /// and return the new note id (its content is filled via [`Self::insert_note_block`]
    /// + [`Self::splice_note_text`]).
    pub fn insert_footnote(&mut self, block_id: &str, char_idx: usize) -> String {
        let id = self.as_mut().insert_footnote(block_id, char_idx);
        self.doc.commit();
        id
    }

    /// Create an endnote, place its reference marker at `char_idx` in `block_id`,
    /// and return the new note id.
    pub fn insert_endnote(&mut self, block_id: &str, char_idx: usize) -> String {
        let id = self.as_mut().insert_endnote(block_id, char_idx);
        self.doc.commit();
        id
    }

    /// Insert a block into a note's content; returns its id (or `None` if the note
    /// id is unknown).
    pub fn insert_note_block(&mut self, note_id: &str, after: Option<&str>, kind: &str) -> Option<String> {
        let id = self.as_mut().insert_note_block(note_id, after, kind);
        self.doc.commit();
        id
    }

    /// Splice text into a note's block.
    pub fn splice_note_text(&mut self, note_id: &str, block_id: &str, char_idx: usize, del: isize, ins: &str) {
        self.as_mut().splice_note_text(note_id, block_id, char_idx, del, ins);
        self.doc.commit();
    }

    /// Add an inline mark over a note's block text.
    pub fn add_note_mark(&mut self, note_id: &str, block_id: &str, start: usize, end: usize, name: &str, value: impl Into<automerge::ScalarValue>) {
        self.as_mut().add_note_mark(note_id, block_id, start, end, name, value);
        self.doc.commit();
    }

    // ---- comments / annotations -------------------------------------------

    /// The comment ids present in the document (sorted, canonical order).
    pub fn comment_ids(&self) -> Vec<String> {
        crate::model::comment_ids(&self.doc, &self.c)
    }

    /// A comment's author display name, or `None` if the id is unknown.
    pub fn comment_author(&self, id: &str) -> Option<String> {
        crate::model::comment_author(&self.doc, &self.c, id)
    }

    /// A comment's date / timestamp string, or `None` if the id is unknown.
    pub fn comment_date(&self, id: &str) -> Option<String> {
        crate::model::comment_date(&self.doc, &self.c, id)
    }

    /// Materialize a comment's content blocks (empty if the id is unknown).
    pub fn comment_blocks(&self, id: &str) -> Vec<crate::snapshot::BlockView> {
        crate::model::materialize_comment_blocks(&self.doc, &self.c, id)
    }

    /// The ordered block ids of a comment's content (empty if absent).
    pub fn comment_blocks_in_order(&self, id: &str) -> Vec<String> {
        crate::model::comment_blocks_in_order(&self.doc, &self.c, id)
    }

    /// The anchored body range `(block_id, start, end)` of a comment, or `None` if its
    /// anchoring `comment` mark is absent.
    pub fn comment_range(&self, id: &str) -> Option<(String, usize, usize)> {
        crate::model::comment_range(&self.doc, &self.c, id)
    }

    /// Materialize every comment into a [`crate::snapshot::CommentView`].
    pub fn comments(&self) -> Vec<crate::snapshot::CommentView> {
        crate::model::materialize_comments(&self.doc, &self.c)
    }

    /// Create a comment authored by `author` with body `text`, anchored to the body
    /// range `[start, end)` of `block_id`; returns the new comment id.
    pub fn add_comment(&mut self, block_id: &str, start: usize, end: usize, author: &str, text: &str) -> String {
        let id = self.as_mut().add_comment(block_id, start, end, author, text);
        self.doc.commit();
        id
    }

    /// Create an empty comment (author + date) with no anchor; fill its content via
    /// [`Self::insert_comment_block`] + [`Self::splice_comment_text`] and anchor it via
    /// [`Self::add_comment_mark`]. Returns the new comment id.
    pub fn insert_comment(&mut self, author: &str, date: &str) -> String {
        let id = self.as_mut().insert_comment(author, date);
        self.doc.commit();
        id
    }

    /// Anchor an existing comment to the body range `[start, end)` of `block_id`.
    pub fn add_comment_mark(&mut self, block_id: &str, start: usize, end: usize, comment_id: &str) {
        self.as_mut().add_comment_mark(block_id, start, end, comment_id);
        self.doc.commit();
    }

    /// Insert a block into a comment's content; returns its id (or `None` if the
    /// comment id is unknown).
    pub fn insert_comment_block(&mut self, comment_id: &str, after: Option<&str>, kind: &str) -> Option<String> {
        let id = self.as_mut().insert_comment_block(comment_id, after, kind);
        self.doc.commit();
        id
    }

    /// Splice text into a comment's block.
    pub fn splice_comment_text(&mut self, comment_id: &str, block_id: &str, char_idx: usize, del: isize, ins: &str) {
        self.as_mut().splice_comment_text(comment_id, block_id, char_idx, del, ins);
        self.doc.commit();
    }

    // ---- tracked changes / revision marks ---------------------------------

    /// The tracked-change ids present in the document (sorted, canonical order).
    pub fn change_ids(&self) -> Vec<String> {
        crate::model::change_ids(&self.doc, &self.c)
    }

    /// A change's kind (`Ins`/`Del`), or `None` if the id is unknown.
    pub fn change_kind(&self, id: &str) -> Option<crate::snapshot::ChangeKind> {
        crate::model::change_kind(&self.doc, &self.c, id)
    }

    /// A change's author display name, or `None` if the id is unknown.
    pub fn change_author(&self, id: &str) -> Option<String> {
        crate::model::change_author(&self.doc, &self.c, id)
    }

    /// A change's date / timestamp string, or `None` if the id is unknown.
    pub fn change_date(&self, id: &str) -> Option<String> {
        crate::model::change_date(&self.doc, &self.c, id)
    }

    /// The anchored body range `(block_id, start, end)` of a change, or `None` if its
    /// anchoring `ins`/`del` mark is absent.
    pub fn change_range(&self, id: &str) -> Option<(String, usize, usize)> {
        crate::model::change_range(&self.doc, &self.c, id)
    }

    /// Materialize every tracked change into a [`crate::snapshot::ChangeView`].
    pub fn changes(&self) -> Vec<crate::snapshot::ChangeView> {
        crate::model::materialize_changes(&self.doc, &self.c)
    }

    /// Track a **insertion**: splice `text` into `block_id` at `char_idx`, flagging the
    /// inserted range as a tracked insertion authored by `author` (dated `date`).
    /// Returns the new change id.
    pub fn track_insert(&mut self, block_id: &str, char_idx: usize, text: &str, author: &str, date: &str) -> String {
        let id = self.as_mut().track_insert(block_id, char_idx, text, author, date);
        self.doc.commit();
        id
    }

    /// Track a **deletion**: flag the existing body range `[start, end)` of `block_id` as
    /// a tracked deletion (retained-but-struck) authored by `author` (dated `date`).
    /// Returns the new change id.
    pub fn track_delete(&mut self, block_id: &str, start: usize, end: usize, author: &str, date: &str) -> String {
        let id = self.as_mut().track_delete(block_id, start, end, author, date);
        self.doc.commit();
        id
    }

    /// Accept a tracked change (insertion: keep text, drop mark; deletion: remove text).
    pub fn accept_change(&mut self, change_id: &str) {
        self.as_mut().accept_change(change_id);
        self.doc.commit();
    }

    /// Reject a tracked change (insertion: remove text; deletion: keep text, drop mark).
    pub fn reject_change(&mut self, change_id: &str) {
        self.as_mut().reject_change(change_id);
        self.doc.commit();
    }

    // ---- bookmarks / cross-references -------------------------------------

    /// Every bookmark (named anchor) in the document, recovered from the body
    /// `bookmark` marks.
    pub fn bookmarks(&self) -> Vec<crate::snapshot::BookmarkView> {
        crate::model::bookmarks(&self.doc, &self.c)
    }

    /// Anchor a bookmark named `name` over the body range `[start, end)` of `block_id`.
    /// A point bookmark (`start == end`) is widened to one character so the mark has a
    /// non-empty range.
    pub fn add_bookmark(&mut self, block_id: &str, start: usize, end: usize, name: &str) {
        self.as_mut().add_bookmark(block_id, start, end, name);
        self.doc.commit();
    }

    /// Anchor a cross-reference (internal link) over the body range `[start, end)` of
    /// `block_id`, pointing at `target` (a bookmark name or a heading's derived id —
    /// see [`crate::model::heading_ref_id`]). Stored as a `link` mark valued
    /// `#<target>`.
    pub fn add_cross_ref(&mut self, block_id: &str, start: usize, end: usize, target: &str) {
        self.as_mut().add_cross_ref(block_id, start, end, target);
        self.doc.commit();
    }

    /// Timeline of authored changes (oldest first).
    pub fn versions(&mut self) -> Vec<crate::history::VersionMeta> {
        crate::history::versions(&mut self.doc)
    }

    /// Reconstruct the document as of `version` (time-travel).
    pub fn materialize_at(&mut self, version: usize) -> Result<crate::snapshot::DocSnapshot, DocError> {
        crate::history::materialize_at(&mut self.doc, version)
    }

    /// Independent replica with a fresh actor id (distinct edit lineage).
    pub fn fork(&mut self) -> Document {
        let bytes = self.save();
        let mut doc = AutoCommit::load(&bytes).expect("fork load");
        doc.set_actor(automerge::ActorId::random());
        let c = crate::model::open(&doc, &ROOT).expect("fork open");
        let ids = IdGen::new(doc.get_actor().to_hex_string());
        Document { doc, root: ROOT, c, ids }
    }

    /// Merge another replica's changes into this one.
    ///
    /// Done via the low-level `Automerge` (no patch log) to sidestep an
    /// automerge-0.8 `AutoCommit::merge` panic (`PatchLogMismatch`) that can fire
    /// when concurrent **mark** ops are reconciled. The merged result is reloaded
    /// as an `AutoCommit`; containers/ids are re-resolved (object ids are stable).
    pub fn merge(&mut self, other: &mut Document) {
        let merged = {
            let mut a = automerge::Automerge::load(&self.doc.save()).expect("merge: load self");
            let mut b = automerge::Automerge::load(&other.doc.save()).expect("merge: load other");
            let _ = a.merge(&mut b);
            a.save()
        };
        self.doc = AutoCommit::load(&merged).expect("merge: reload");
        self.c = crate::model::open(&self.doc, &ROOT).expect("merge: open");
        self.ids = crate::model::seeded_id_gen(&self.doc, &self.c);
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::*;
    use automerge::{ObjType, ReadDoc, Value};

    #[test]
    fn new_doc_has_root_layout() {
        let d = Document::new();
        let (v, _) = d.doc.get(&ROOT, KEY_SCHEMA).unwrap().unwrap();
        assert_eq!(v.to_str(), Some(SCHEMA_ID));
        let (vv, _) = d.doc.get(&ROOT, KEY_SCHEMA_VERSION).unwrap().unwrap();
        assert_eq!(vv.to_i64(), Some(SCHEMA_VERSION));
        for k in [KEY_BLOCKS, KEY_STYLES, KEY_PRESERVED, KEY_META, KEY_BLOBS] {
            let got = d.doc.get(&ROOT, k).unwrap();
            assert!(matches!(got, Some((Value::Object(ObjType::Map), _))), "missing map {k}");
        }
    }

    #[test]
    fn save_load_roundtrips() {
        let mut d = Document::new();
        let bytes = d.save();
        let d2 = Document::load(&bytes).unwrap();
        let (v, _) = d2.doc.get(&ROOT, KEY_SCHEMA).unwrap().unwrap();
        assert_eq!(v.to_str(), Some(SCHEMA_ID));
    }

    #[test]
    fn columns_default_set_clear_and_persist() {
        let mut d = Document::new();
        // Absent → single column, surfaced in the snapshot.
        assert_eq!(d.columns(), (1, None), "default is a single column with no gap");
        assert_eq!(d.materialize().columns, 1);
        assert_eq!(d.materialize().column_gap, None);

        // Set + read back, and surface in the snapshot.
        d.set_columns(2, Some(18.0));
        assert_eq!(d.columns(), (2, Some(18.0)));
        let snap = d.materialize();
        assert_eq!(snap.columns, 2);
        assert_eq!(snap.column_gap, Some(18.0));

        // Survives a CRDT save/load round-trip.
        let bytes = d.save();
        let d2 = Document::load(&bytes).unwrap();
        assert_eq!(d2.columns(), (2, Some(18.0)), "columns survive save/load");

        // Setting back to one column clears the setting (single-column default).
        let mut d3 = Document::load(&bytes).unwrap();
        d3.set_columns(1, None);
        assert_eq!(d3.columns(), (1, None));
        assert_eq!(d3.materialize().columns, 1);
    }

    #[test]
    fn columns_count_is_clamped_to_at_least_one() {
        let mut d = Document::new();
        // A zero count is meaningless; the reader clamps to a single column.
        d.set_columns(0, None);
        assert_eq!(d.columns(), (1, None));
    }

    #[test]
    fn docsnapshot_default_is_single_column() {
        // The hand-rolled `Default` must yield one column, not the integer default 0.
        assert_eq!(crate::snapshot::DocSnapshot::default().columns, 1);
    }

    #[test]
    fn insert_blocks_are_ordered() {
        let mut d = Document::new();
        let a = d.insert_block(None, "paragraph");
        let b = d.insert_block(Some(&a), "paragraph");
        let c = d.insert_block(Some(&a), "paragraph"); // between a and b
        assert_eq!(d.blocks_in_order(), vec![a.clone(), c.clone(), b.clone()]);
        assert_eq!(d.block_kind(&c).as_deref(), Some("paragraph"));
    }

    #[test]
    fn remove_and_move_block() {
        let mut d = Document::new();
        let a = d.insert_block(None, "paragraph");
        let b = d.insert_block(Some(&a), "paragraph");
        let c = d.insert_block(Some(&b), "paragraph");
        d.remove_block(&b);
        assert_eq!(d.blocks_in_order(), vec![a.clone(), c.clone()]);
        d.move_block(&c, None); // to front
        assert_eq!(d.blocks_in_order(), vec![c, a]);
    }

    #[test]
    fn text_splice_and_read() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "hello world");
        assert_eq!(d.block_text(&p).as_deref(), Some("hello world"));
        d.splice_text(&p, 5, 0, ",");
        assert_eq!(d.block_text(&p).as_deref(), Some("hello, world"));
        d.splice_text(&p, 0, 5, "HELLO");
        assert_eq!(d.block_text(&p).as_deref(), Some("HELLO, world"));
    }

    #[test]
    fn text_handles_unicode() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "héllo");
        assert_eq!(d.block_text(&p).as_deref(), Some("héllo"));
    }

    #[test]
    fn marks_apply_over_range() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "hello world");
        d.add_mark(&p, 0, 5, "strong", true);
        let m = d.marks(&p);
        assert!(
            m.iter().any(|s| s.name == "strong" && s.start == 0 && s.end == 5),
            "expected strong[0,5], got {m:?}"
        );
    }

    #[test]
    fn mark_survives_later_text_insert() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "hello world");
        d.add_mark(&p, 6, 11, "em", true); // "world"
        d.splice_text(&p, 0, 0, "say "); // shift right by 4
        let m = d.marks(&p);
        assert!(
            m.iter().any(|s| s.name == "em" && s.start == 10 && s.end == 15),
            "mark must track text shift, got {m:?}"
        );
    }

    #[test]
    fn materialize_reflects_model() {
        let mut d = Document::new();
        let a = d.insert_block(None, "heading");
        d.splice_text(&a, 0, 0, "Title");
        let b = d.insert_block(Some(&a), "paragraph");
        d.splice_text(&b, 0, 0, "Body");
        d.add_mark(&b, 0, 4, "strong", true);

        let snap = d.materialize();
        assert_eq!(snap.schema, SCHEMA_ID);
        assert_eq!(snap.blocks.len(), 2);
        assert_eq!(snap.blocks[0].kind, "heading");
        assert_eq!(snap.blocks[0].text, "Title");
        assert_eq!(snap.blocks[1].text, "Body");
        assert!(snap.blocks[1].marks.iter().any(|m| m.name == "strong"));
    }

    #[test]
    fn seeded_id_gen_avoids_collision_over_existing_doc() {
        let mut d = Document::new();
        let a = d.insert_block(None, "paragraph");
        d.splice_text(&a, 0, 0, "first");
        // Re-create a generator over the same doc + actor (as the editor does
        // when binding to a loaded document). A fresh IdGen at 0 would re-mint
        // `a`'s id and overwrite it; the seeded one must not.
        let c = d.c.clone();
        let mut ids = crate::model::seeded_id_gen(&d.doc, &c);
        {
            let mut dm = DocMut { doc: &mut d.doc, c: &c, ids: &mut ids };
            let b = dm.insert_block(Some(&a), "paragraph");
            assert_ne!(a, b, "minted id must differ from the existing block");
            dm.splice_text(&b, 0, 0, "second");
        }
        let snap = d.materialize();
        assert_eq!(snap.blocks.len(), 2, "must not overwrite the existing block");
        assert_eq!(snap.blocks[0].text, "first");
        assert_eq!(snap.blocks[1].text, "second");
    }

    #[test]
    fn footnote_accessors_and_snapshot() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "see ref");
        let nid = d.insert_footnote(&p, 4);
        let nb = d.insert_note_block(&nid, None, "paragraph").unwrap();
        d.splice_note_text(&nid, &nb, 0, 0, "footnote body");

        // Accessors agree with the snapshot.
        assert_eq!(d.note_ids(), vec![nid.clone()]);
        assert_eq!(d.note_kind(&nid), Some(crate::model::NoteKind::Footnote));
        assert_eq!(d.note_blocks(&nid)[0].text, "footnote body");
        let snap = d.materialize();
        assert_eq!(snap.notes.len(), 1);
        assert_eq!(snap.notes[0].id, nid);
        // The note-ref marker survives a save/load round-trip (seeded id gen).
        let bytes = d.save();
        let d2 = Document::load(&bytes).unwrap();
        assert_eq!(d2.note_blocks(&nid)[0].text, "footnote body");
        assert!(d2.materialize().blocks[0].marks.iter().any(|m| m.name == crate::schema::MARK_NOTE_REF));
    }

    #[test]
    fn notes_converge_across_replicas() {
        // Two replicas each add a note to the same document; after merge both
        // converge with both notes present (CRDT-native).
        let mut a = Document::new();
        let p = a.insert_block(None, "paragraph");
        a.splice_text(&p, 0, 0, "base");
        let mut b = a.fork();
        let na = a.insert_footnote(&p, 4);
        let nab = a.insert_note_block(&na, None, "paragraph").unwrap();
        a.splice_note_text(&na, &nab, 0, 0, "from A");
        let nb = b.insert_endnote(&p, 0);
        let nbb = b.insert_note_block(&nb, None, "paragraph").unwrap();
        b.splice_note_text(&nb, &nbb, 0, 0, "from B");
        a.merge(&mut b);
        b.merge(&mut a);
        assert_eq!(a.materialize(), b.materialize(), "replicas converge with notes");
        assert_eq!(a.materialize().notes.len(), 2, "both notes present after merge");
    }

    #[test]
    fn comment_accessors_and_snapshot() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "review this paragraph");
        let cid = d.add_comment(&p, 7, 11, "Grace", "note: fix"); // over "this"

        // Accessors agree with the snapshot.
        assert_eq!(d.comment_ids(), vec![cid.clone()]);
        assert_eq!(d.comment_author(&cid).as_deref(), Some("Grace"));
        assert_eq!(d.comment_blocks(&cid)[0].text, "note: fix");
        assert_eq!(d.comment_range(&cid), Some((p.clone(), 7, 11)));
        let snap = d.materialize();
        assert_eq!(snap.comments.len(), 1);
        assert_eq!(snap.comments[0].id, cid);
        assert_eq!(snap.comments[0].range, Some((p.clone(), 7, 11)));
        // The comment mark + content survive a save/load round-trip (seeded id gen).
        let bytes = d.save();
        let d2 = Document::load(&bytes).unwrap();
        assert_eq!(d2.comment_blocks(&cid)[0].text, "note: fix");
        assert!(d2.materialize().blocks[0].marks.iter().any(|m| m.name == crate::schema::MARK_COMMENT));
    }

    #[test]
    fn comments_converge_across_replicas() {
        // Two replicas each add a comment to the same document; after merge both
        // converge with both comments present (CRDT-native).
        let mut a = Document::new();
        let p = a.insert_block(None, "paragraph");
        a.splice_text(&p, 0, 0, "base text here");
        let mut b = a.fork();
        a.add_comment(&p, 0, 4, "A", "from A"); // over "base"
        b.add_comment(&p, 5, 9, "B", "from B"); // over "text"
        a.merge(&mut b);
        b.merge(&mut a);
        assert_eq!(a.materialize(), b.materialize(), "replicas converge with comments");
        assert_eq!(a.materialize().comments.len(), 2, "both comments present after merge");
    }

    #[test]
    fn tracked_change_accessors_and_save_load() {
        let mut d = Document::new();
        let p = d.insert_block(None, "paragraph");
        d.splice_text(&p, 0, 0, "the quick fox");
        // Track an insertion of "brown " before "fox" (idx 10).
        let ins = d.track_insert(&p, 10, "brown ", "Grace", "2021-05-06");
        // Track a deletion of "quick " (chars 4..10).
        let del = d.track_delete(&p, 4, 10, "Hopper", "2021-07-08");

        // Accessors agree.
        let mut want = vec![ins.clone(), del.clone()];
        want.sort();
        assert_eq!(d.change_ids(), want);
        assert_eq!(d.change_kind(&ins), Some(crate::snapshot::ChangeKind::Ins));
        assert_eq!(d.change_kind(&del), Some(crate::snapshot::ChangeKind::Del));
        assert_eq!(d.change_author(&ins).as_deref(), Some("Grace"));
        assert_eq!(d.change_date(&del).as_deref(), Some("2021-07-08"));
        assert!(d.materialize().changes.len() == 2);

        // Marks + records survive a save/load round-trip (seeded id gen).
        let bytes = d.save();
        let d2 = Document::load(&bytes).unwrap();
        let snap = d2.materialize();
        assert_eq!(snap.changes.len(), 2, "changes survive save/load");
        assert!(snap.blocks[0].marks.iter().any(|m| m.name == crate::schema::MARK_INS));
        assert!(snap.blocks[0].marks.iter().any(|m| m.name == crate::schema::MARK_DEL));
    }

    #[test]
    fn tracked_changes_converge_across_replicas() {
        // Two replicas each track a change on the same document; after merge both
        // converge with both changes present (CRDT-native).
        let mut a = Document::new();
        let p = a.insert_block(None, "paragraph");
        a.splice_text(&p, 0, 0, "shared base text");
        let mut b = a.fork();
        a.track_delete(&p, 0, 6, "A", ""); // "shared"
        b.track_insert(&p, 16, "!", "B", "");
        a.merge(&mut b);
        b.merge(&mut a);
        assert_eq!(a.materialize(), b.materialize(), "replicas converge with tracked changes");
        assert_eq!(a.materialize().changes.len(), 2, "both changes present after merge");
    }

    #[test]
    fn merge_combines_disjoint_edits() {
        let mut a = Document::new();
        let p = a.insert_block(None, "paragraph");
        a.splice_text(&p, 0, 0, "base");
        let mut b = a.fork();
        a.splice_text(&p, 4, 0, "-A");
        let q = b.insert_block(Some(&p), "paragraph");
        b.splice_text(&q, 0, 0, "B-block");
        a.merge(&mut b);
        b.merge(&mut a);
        assert_eq!(a.materialize(), b.materialize(), "replicas must converge");
        assert_eq!(a.blocks_in_order().len(), 2);
    }
}
