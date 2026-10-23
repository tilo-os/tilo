//! `Document`: an owned handle over an automerge `AutoCommit` holding the canonical rich-text
//! model. Thin wrapper delegating to [`crate::model`]; the borrowed [`crate::model::DocMut`] view
//! drives the same operations over a host's doc (e.g. `ui::State.doc`).

use crate::model::{Containers, DocMut, DocRef, IdGen, IdGenError};
use automerge::{AutoCommit, ROOT};
use tilo_sort::CompactSort as _;

#[derive(thiserror::Error, Debug)]
pub enum DocError {
    #[error("automerge error: {0}")]
    Automerge(#[from] automerge::AutomergeError),
    #[error("load error: {0}")]
    Load(String),
    #[error("invalid document: {0}")]
    Invalid(String),
    #[error(transparent)]
    IdNamespaceExhausted(#[from] IdGenError),
}

/// The value carried by a mark. Boolean marks (`strong`/`em`/`underline`) carry `Bool(true)`;
/// valued marks (`fontSize`/`fontFamily`/`link`/`color`) carry `Int`/`Str`. `Ord` is derived so
/// materialization can canonicalise mark order *including the value*, so replicas that merged
/// concurrent valued marks in different internal orders converge to a byte-identical span list.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
    tilo_resource::Model,
)]
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
            ScalarValue::Uint(u) => MarkValue::Int(i64::try_from(*u).unwrap_or(i64::MAX)),
            ScalarValue::Str(s) => MarkValue::Str(s.to_string()),
            _ => MarkValue::Bool(true),
        }
    }
}

/// One inline formatting span as seen from the read side.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    tilo_resource::JsonSchema,
    tilo_resource::Model,
)]
pub struct MarkSpan {
    pub start: usize,
    pub end: usize,
    #[model(atom)]
    pub name: String,
    pub value: MarkValue,
}

/// The authored runs `marks` records, as `(start, end, author)` merged where adjacent runs name
/// the same author. One reader for the durable half of authorship, so renderer and test can't
/// drift. Spans merge because a run typed in several sittings is one person's paragraph, not
/// three — automerge splits a mark wherever a concurrent edit landed inside it.
pub fn author_runs(marks: &[MarkSpan]) -> Vec<(usize, usize, String)> {
    let mut runs: Vec<(usize, usize, String)> = marks
        .iter()
        .filter(|m| m.name == crate::schema::MARK_AUTHOR && m.end > m.start)
        .filter_map(|m| m.value.as_str().map(|a| (m.start, m.end, a.to_string())))
        .filter(|(_, _, a)| !a.is_empty())
        .collect();
    runs.compact_sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    let mut merged: Vec<(usize, usize, String)> = Vec::with_capacity(runs.len());
    for (start, end, author) in runs {
        match merged.last_mut() {
            Some(prev) if prev.2 == author && prev.1 >= start => {
                prev.1 = prev.1.max(end);
            }
            _ => merged.push((start, end, author)),
        }
    }
    merged
}

pub struct Document {
    pub(crate) doc: AutoCommit,
    pub(crate) c: Containers,
    ids: IdGen,
}

impl Document {
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        let c = crate::model::init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        ids.allow_commits();
        doc.commit();
        Self { doc, c, ids }
    }

    /// # Errors
    /// Returns [`DocError`] if `bytes` is not a valid automerge document, or if it does not
    /// carry a tilo-doc schema `open` recognises.
    pub fn load(bytes: &[u8]) -> Result<Self, DocError> {
        let doc = AutoCommit::load(bytes).map_err(|e| DocError::Load(e.to_string()))?;
        Self::from_automerge(doc)
    }

    /// Open an Automerge document that is already loaded, as [`Self::load`] does: for bytes
    /// from peers, which are decoded in `tilo_automerge_isolate` rather than here.
    ///
    /// # Errors
    /// Returns [`DocError`] if `doc` does not carry a tilo-doc schema `open` recognises.
    pub fn from_automerge(doc: AutoCommit) -> Result<Self, DocError> {
        Self::from_automerge_under(doc, &ROOT)
    }

    /// [`Self::from_automerge`] for a document whose rich text is stored
    /// under `root`, the map of a field of a larger record.
    ///
    /// # Errors
    /// Returns [`DocError`] if `root` does not hold a tilo-doc schema `open` recognises.
    pub fn from_automerge_under(
        doc: AutoCommit,
        root: &automerge::ObjId,
    ) -> Result<Self, DocError> {
        let c = crate::model::open(&doc, root)?;
        let mut ids = crate::model::try_seeded_id_gen(&doc, &c)?;
        ids.allow_commits();
        Ok(Self { doc, c, ids })
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    pub fn actor_hex(&self) -> String {
        self.doc.get_actor().to_hex_string()
    }

    /// Borrowed mutable view (for codecs / shared editing paths).
    pub fn as_mut(&mut self) -> DocMut<'_> {
        DocMut {
            doc: &mut self.doc,
            c: &self.c,
            ids: &mut self.ids,
        }
    }

    /// Borrowed read-only view (for codec export).
    pub fn as_ref(&self) -> DocRef<'_> {
        DocRef {
            doc: &self.doc,
            c: &self.c,
        }
    }

    // The owned `Document` commits per op so its change history (timeline) stays fine-grained (borrowed `DocMut` does not — see model.rs).
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

    pub fn add_mark(
        &mut self,
        block_id: &str,
        start: usize,
        end: usize,
        name: &str,
        value: impl Into<automerge::ScalarValue>,
    ) {
        self.as_mut().add_mark(block_id, start, end, name, value);
        self.doc.commit();
    }

    /// Take `bytes` as a binary input of this document (an image to place): they go into `inputs`,
    /// which the caller persists in its content store, and the document records their descriptor.
    /// Returns the content address an `image` block's `blob` attr names.
    pub fn attach(&mut self, bytes: &[u8], inputs: &mut tilo_content::ContentInputs) -> String {
        let hash = self.as_mut().attach(bytes, inputs);
        self.doc.commit();
        hash
    }

    /// Record that the document refers to content the caller already keeps (see
    /// [`crate::model::DocMut::reference_content`]).
    pub fn reference_content(&mut self, hash: &str, meta: &tilo_content::ContentMeta) {
        self.as_mut().reference_content(hash, meta);
        self.doc.commit();
    }

    /// Every content address the document refers to, in any version; see
    /// [`crate::model::referenced_content`].
    pub fn referenced_content(&self) -> std::collections::BTreeSet<String> {
        crate::model::referenced_content(&self.doc, &self.c)
    }

    /// Set a document property (`title`/`author`/`subject`/`keywords`/`created`/`modified`/`generator`). Round-trips through `.odt`/`.docx`.
    pub fn set_doc_prop(&mut self, name: &str, value: &str) {
        self.as_mut().set_doc_prop(name, value);
        self.doc.commit();
    }

    /// Read a single document property, or `None` if unset.
    pub fn doc_prop(&self, name: &str) -> Option<String> {
        crate::model::doc_prop(&self.doc, &self.c, name)
    }

    pub fn doc_props(&self) -> std::collections::BTreeMap<String, String> {
        crate::model::doc_props(&self.doc, &self.c)
    }

    /// Set the document-wide multi-column layout: `count` columns for the body, optional `gap` (px). `count <= 1` clears the setting.
    pub fn set_columns(&mut self, count: u32, gap: Option<f64>) {
        self.as_mut().set_columns(count, gap);
        self.doc.commit();
    }

    /// The document-wide multi-column layout: `(column_count, optional_gap_px)`, defaulting to `(1, None)`.
    pub fn columns(&self) -> (u32, Option<f64>) {
        crate::model::columns(&self.doc, &self.c)
    }

    /// Set the document default language as a BCP-47 tag (`en-US`, `de-DE`, `fr`); an empty `code` clears it.
    pub fn set_language(&mut self, code: &str) {
        self.as_mut().set_language(code);
        self.doc.commit();
    }

    pub fn language(&self) -> Option<String> {
        crate::model::language(&self.doc, &self.c)
    }

    pub fn blocks_in_order(&self) -> Vec<String> {
        crate::model::blocks_in_order(&self.doc, &self.c)
    }

    /// The byte length of every body block's fractional order key — a direct probe of the truncating
    /// order-key scheme; the perf test asserts it stays bounded rather than growing O(n) per append.
    pub fn block_order_key_lengths(&self) -> Vec<usize> {
        crate::model::block_order_key_lengths(&self.doc, &self.c)
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

    pub fn header_blocks_in_order(&self) -> Vec<String> {
        crate::model::region_blocks_in_order(&self.doc, &self.c, crate::model::Region::Header)
    }

    pub fn footer_blocks_in_order(&self) -> Vec<String> {
        crate::model::region_blocks_in_order(&self.doc, &self.c, crate::model::Region::Footer)
    }

    pub fn header(&self) -> Vec<crate::snapshot::BlockView> {
        crate::model::materialize_region(&self.doc, &self.c, crate::model::Region::Header)
    }

    pub fn footer(&self) -> Vec<crate::snapshot::BlockView> {
        crate::model::materialize_region(&self.doc, &self.c, crate::model::Region::Footer)
    }

    pub fn insert_header_block(&mut self, after: Option<&str>, kind: &str) -> String {
        let id = self
            .as_mut()
            .insert_block_in(crate::model::Region::Header, after, kind);
        self.doc.commit();
        id
    }

    pub fn insert_footer_block(&mut self, after: Option<&str>, kind: &str) -> String {
        let id = self
            .as_mut()
            .insert_block_in(crate::model::Region::Footer, after, kind);
        self.doc.commit();
        id
    }

    pub fn splice_header_text(&mut self, block_id: &str, char_idx: usize, del: isize, ins: &str) {
        self.as_mut()
            .splice_text_in(crate::model::Region::Header, block_id, char_idx, del, ins);
        self.doc.commit();
    }

    pub fn splice_footer_text(&mut self, block_id: &str, char_idx: usize, del: isize, ins: &str) {
        self.as_mut()
            .splice_text_in(crate::model::Region::Footer, block_id, char_idx, del, ins);
        self.doc.commit();
    }

    pub fn insert_footer_page_number(&mut self, block_id: &str, char_idx: usize) {
        self.as_mut()
            .insert_page_number_field(crate::model::Region::Footer, block_id, char_idx);
        self.doc.commit();
    }

    /// The note ids present in the document (sorted, canonical order).
    pub fn note_ids(&self) -> Vec<String> {
        crate::model::note_ids(&self.doc, &self.c)
    }

    /// A note's kind (`Footnote`/`Endnote`), or `None` if the id is unknown.
    pub fn note_kind(&self, id: &str) -> Option<crate::model::NoteKind> {
        crate::model::note_kind(&self.doc, &self.c, id)
    }

    pub fn note_blocks(&self, id: &str) -> Vec<crate::snapshot::BlockView> {
        crate::model::materialize_note_blocks(&self.doc, &self.c, id)
    }

    pub fn note_blocks_in_order(&self, id: &str) -> Vec<String> {
        crate::model::note_blocks_in_order(&self.doc, &self.c, id)
    }

    pub fn notes(&self) -> Vec<crate::snapshot::NoteView> {
        crate::model::materialize_notes(&self.doc, &self.c)
    }

    /// Create a footnote, place its reference marker at `char_idx` in `block_id`, and return the new
    /// note id (content filled via [`Self::insert_note_block`] + [`Self::splice_note_text`]).
    pub fn insert_footnote(&mut self, block_id: &str, char_idx: usize) -> String {
        let id = self.as_mut().insert_footnote(block_id, char_idx);
        self.doc.commit();
        id
    }

    pub fn insert_endnote(&mut self, block_id: &str, char_idx: usize) -> String {
        let id = self.as_mut().insert_endnote(block_id, char_idx);
        self.doc.commit();
        id
    }

    pub fn insert_note_block(
        &mut self,
        note_id: &str,
        after: Option<&str>,
        kind: &str,
    ) -> Option<String> {
        let id = self.as_mut().insert_note_block(note_id, after, kind);
        self.doc.commit();
        id
    }

    pub fn splice_note_text(
        &mut self,
        note_id: &str,
        block_id: &str,
        char_idx: usize,
        del: isize,
        ins: &str,
    ) {
        self.as_mut()
            .splice_note_text(note_id, block_id, char_idx, del, ins);
        self.doc.commit();
    }

    pub fn add_note_mark(
        &mut self,
        note_id: &str,
        block_id: &str,
        start: usize,
        end: usize,
        name: &str,
        value: impl Into<automerge::ScalarValue>,
    ) {
        self.as_mut()
            .add_note_mark(note_id, block_id, start, end, name, value);
        self.doc.commit();
    }

    /// The comment ids present in the document (sorted, canonical order).
    pub fn comment_ids(&self) -> Vec<String> {
        crate::model::comment_ids(&self.doc, &self.c)
    }

    pub fn comment_author(&self, id: &str) -> Option<String> {
        crate::model::comment_author(&self.doc, &self.c, id)
    }

    pub fn comment_date(&self, id: &str) -> Option<String> {
        crate::model::comment_date(&self.doc, &self.c, id)
    }

    pub fn comment_blocks(&self, id: &str) -> Vec<crate::snapshot::BlockView> {
        crate::model::materialize_comment_blocks(&self.doc, &self.c, id)
    }

    pub fn comment_blocks_in_order(&self, id: &str) -> Vec<String> {
        crate::model::comment_blocks_in_order(&self.doc, &self.c, id)
    }

    pub fn comment_range(&self, id: &str) -> Option<(String, usize, usize)> {
        crate::model::comment_range(&self.doc, &self.c, id)
    }

    pub fn comments(&self) -> Vec<crate::snapshot::CommentView> {
        crate::model::materialize_comments(&self.doc, &self.c)
    }

    /// Materialize the durable replies below one comment, in canonical id
    /// order. An old comment without a reply map simply has no replies.
    pub fn comment_replies(&self, comment_id: &str) -> Vec<crate::snapshot::CommentReplyView> {
        crate::model::materialize_comment_replies(&self.doc, &self.c, comment_id)
    }

    pub fn add_comment(
        &mut self,
        block_id: &str,
        start: usize,
        end: usize,
        author: &str,
        text: &str,
    ) -> String {
        let id = self
            .as_mut()
            .add_comment(block_id, start, end, author, text);
        self.doc.commit();
        id
    }

    /// Create an empty comment (author + date) with no anchor; fill its content via
    /// [`Self::insert_comment_block`] + [`Self::splice_comment_text`] and anchor via [`Self::add_comment_mark`].
    pub fn insert_comment(&mut self, author: &str, date: &str) -> String {
        let id = self.as_mut().insert_comment(author, date);
        self.doc.commit();
        id
    }

    pub fn add_comment_mark(&mut self, block_id: &str, start: usize, end: usize, comment_id: &str) {
        self.as_mut()
            .add_comment_mark(block_id, start, end, comment_id);
        self.doc.commit();
    }

    /// Add a one-paragraph reply to an existing comment thread. Returns `None`
    /// when a concurrent deletion removed the parent comment before this write.
    pub fn add_comment_reply(
        &mut self,
        comment_id: &str,
        author: &str,
        date: &str,
        text: &str,
    ) -> Option<String> {
        let id = self
            .as_mut()
            .add_comment_reply(comment_id, author, date, text);
        if id.is_some() {
            self.doc.commit();
        }
        id
    }

    pub fn insert_comment_block(
        &mut self,
        comment_id: &str,
        after: Option<&str>,
        kind: &str,
    ) -> Option<String> {
        let id = self.as_mut().insert_comment_block(comment_id, after, kind);
        self.doc.commit();
        id
    }

    pub fn splice_comment_text(
        &mut self,
        comment_id: &str,
        block_id: &str,
        char_idx: usize,
        del: isize,
        ins: &str,
    ) {
        self.as_mut()
            .splice_comment_text(comment_id, block_id, char_idx, del, ins);
        self.doc.commit();
    }

    /// The tracked-change ids present in the document (sorted, canonical order).
    pub fn change_ids(&self) -> Vec<String> {
        crate::model::change_ids(&self.doc, &self.c)
    }

    pub fn change_kind(&self, id: &str) -> Option<crate::snapshot::ChangeKind> {
        crate::model::change_kind(&self.doc, &self.c, id)
    }

    pub fn change_author(&self, id: &str) -> Option<String> {
        crate::model::change_author(&self.doc, &self.c, id)
    }

    pub fn change_date(&self, id: &str) -> Option<String> {
        crate::model::change_date(&self.doc, &self.c, id)
    }

    pub fn change_range(&self, id: &str) -> Option<(String, usize, usize)> {
        crate::model::change_range(&self.doc, &self.c, id)
    }

    pub fn changes(&self) -> Vec<crate::snapshot::ChangeView> {
        crate::model::materialize_changes(&self.doc, &self.c)
    }

    pub fn track_insert(
        &mut self,
        block_id: &str,
        char_idx: usize,
        text: &str,
        author: &str,
        date: &str,
    ) -> String {
        let id = self
            .as_mut()
            .track_insert(block_id, char_idx, text, author, date);
        self.doc.commit();
        id
    }

    pub fn track_delete(
        &mut self,
        block_id: &str,
        start: usize,
        end: usize,
        author: &str,
        date: &str,
    ) -> String {
        let id = self
            .as_mut()
            .track_delete(block_id, start, end, author, date);
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

    pub fn bookmarks(&self) -> Vec<crate::snapshot::BookmarkView> {
        crate::model::bookmarks(&self.doc, &self.c)
    }

    /// Anchor a bookmark named `name` over `[start, end)`; a point bookmark (`start == end`) widens to one character so the mark has a non-empty range.
    pub fn add_bookmark(&mut self, block_id: &str, start: usize, end: usize, name: &str) {
        self.as_mut().add_bookmark(block_id, start, end, name);
        self.doc.commit();
    }

    /// Anchor a cross-reference over `[start, end)`, pointing at `target` (a bookmark name or a
    /// heading's derived id); stored as a `link` mark valued `#<target>`.
    pub fn add_cross_ref(&mut self, block_id: &str, start: usize, end: usize, target: &str) {
        self.as_mut().add_cross_ref(block_id, start, end, target);
        self.doc.commit();
    }

    /// Timeline of authored changes (oldest first).
    pub fn versions(&mut self) -> Vec<tilo_resource::history::Version> {
        tilo_resource::history::versions(&mut self.doc)
    }

    /// Reconstruct the document as of `version` (time-travel).
    ///
    /// # Errors
    /// Returns [`DocError`] if `version` is out of range or the historical state cannot be
    /// replayed.
    pub fn materialize_at(
        &mut self,
        version: usize,
    ) -> Result<crate::snapshot::DocSnapshot, DocError> {
        crate::snapshot::at(&mut self.doc, version)
    }

    /// Structural diff between two points on this document's own timeline (`from` → `to`, indices
    /// into [`Self::versions`]); block ids are stable across history, so the diff aligns by identity.
    ///
    /// # Errors
    /// Returns [`DocError`] if either `from` or `to` is out of range.
    pub fn diff_versions(
        &mut self,
        from: usize,
        to: usize,
    ) -> Result<crate::diff::DocDiff, DocError> {
        let a = self.materialize_at(from)?;
        let b = self.materialize_at(to)?;
        Ok(crate::diff::diff_snapshots(&a, &b))
    }

    /// Independent replica with a fresh actor id (distinct edit lineage).
    ///
    /// # Panics
    /// Panics if reloading this document's own just-saved bytes fails. `bytes` is this
    /// process's own valid in-memory document round-tripped through `save`/`load`, not
    /// anything read from disk or a peer, so automerge failing to parse it back would mean
    /// `save`/`load` themselves are broken — a library invariant, not bad input. `fork`'s
    /// only real external caller (`fs::docstore`) depends on this infallible signature, so
    /// surfacing a `Result` here is a public-API change out of scope for a lint-only pass.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "reloads this process's own just-saved bytes; see the panics note above"
    )]
    pub fn fork(&mut self) -> Document {
        let bytes = self.save();
        let mut doc = AutoCommit::load(&bytes).expect("fork load");
        doc.set_actor(automerge::ActorId::random());
        let c = crate::model::open(&doc, &ROOT).expect("fork open");
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        ids.allow_commits();
        Document { doc, c, ids }
    }

    /// Merge another replica's changes into this one. Done via the low-level `Automerge` (no patch
    /// log) to sidestep an automerge-0.8 `AutoCommit::merge` panic (`PatchLogMismatch`) that can fire
    /// when concurrent **mark** ops are reconciled; the merged result is reloaded and containers/ids re-resolved.
    ///
    /// # Panics
    /// Panics if reloading either side's own just-saved bytes, or the merged result, fails.
    /// Both `self` and `other` are already-valid in-memory documents (never raw peer bytes —
    /// those go through the fallible `Document::load`), so a failure here means `save`/`load`
    /// disagree with each other, a library invariant rather than untrusted input. `merge`'s
    /// only real external caller (`fs::docstore`) depends on this infallible signature, so
    /// surfacing a `Result` here is a public-API change out of scope for a lint-only pass.
    #[allow(
        clippy::expect_used,
        reason = "reloads this process's own just-saved/merged bytes; see the panics note above"
    )]
    pub fn merge(&mut self, other: &mut Document) {
        let merged = {
            let mut a = automerge::Automerge::load(&self.doc.save()).expect("merge: load self");
            let mut b = automerge::Automerge::load(&other.doc.save()).expect("merge: load other");
            let _ = a.merge(&mut b);
            a.save()
        };
        let candidate_doc = AutoCommit::load(&merged).expect("merge: reload");
        let candidate_containers = crate::model::open(&candidate_doc, &ROOT).expect("merge: open");
        let mut candidate_ids =
            crate::model::try_seeded_id_gen(&candidate_doc, &candidate_containers).expect(
                "merged tilo-doc exhausted its id namespace; refusing to reuse an identifier",
            );
        candidate_ids.allow_commits();
        self.doc = candidate_doc;
        self.c = candidate_containers;
        self.ids = candidate_ids;
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
    fn new_documents_have_the_root_layout_and_survive_save_load() {
        {
            let d = Document::new();
            let (v, _) = d.doc.get(&ROOT, KEY_SCHEMA).unwrap().unwrap();
            assert_eq!(v.as_str(), Some(SCHEMA_ID));
            let (vv, _) = d.doc.get(&ROOT, KEY_SCHEMA_VERSION).unwrap().unwrap();
            assert_eq!(vv.as_i64(), Some(SCHEMA_VERSION));
            for k in [KEY_BLOCKS, KEY_STYLES, KEY_PRESERVED, KEY_META, KEY_BLOBS] {
                let got = d.doc.get(&ROOT, k).unwrap();
                assert!(
                    matches!(got, Some((Value::Object(ObjType::Map), _))),
                    "missing map {k}"
                );
            }
        }
        {
            let mut d = Document::new();
            let bytes = d.save();
            let d2 = Document::load(&bytes).unwrap();
            let (v, _) = d2.doc.get(&ROOT, KEY_SCHEMA).unwrap().unwrap();
            assert_eq!(v.as_str(), Some(SCHEMA_ID));
        }
    }

    #[test]
    fn column_and_language_settings_default_set_clear_and_persist() {
        {
            let mut d = Document::new();
            assert_eq!(
                d.columns(),
                (1, None),
                "default is a single column with no gap"
            );
            assert_eq!(d.materialize().columns, 1);
            assert_eq!(d.materialize().column_gap, None);

            d.set_columns(2, Some(18.0));
            assert_eq!(d.columns(), (2, Some(18.0)));
            let snap = d.materialize();
            assert_eq!(snap.columns, 2);
            assert_eq!(snap.column_gap, Some(18.0));

            let bytes = d.save();
            let d2 = Document::load(&bytes).unwrap();
            assert_eq!(d2.columns(), (2, Some(18.0)), "columns survive save/load");

            // Setting back to one column clears the setting (single-column default).
            let mut d3 = Document::load(&bytes).unwrap();
            d3.set_columns(1, None);
            assert_eq!(d3.columns(), (1, None));
            assert_eq!(d3.materialize().columns, 1);
        }
        {
            let mut d = Document::new();
            // A zero count is meaningless; the reader clamps to a single column.
            d.set_columns(0, None);
            assert_eq!(d.columns(), (1, None));
        }
        {
            // The hand-rolled `Default` must yield one column, not the integer default 0.
            assert_eq!(crate::snapshot::DocSnapshot::default().columns, 1);
        }
        {
            let mut d = Document::new();
            assert_eq!(d.language(), None, "default has no declared language");
            assert_eq!(d.materialize().language, None);

            d.set_language("de-DE");
            assert_eq!(d.language().as_deref(), Some("de-DE"));
            assert_eq!(d.materialize().language.as_deref(), Some("de-DE"));

            let bytes = d.save();
            let d2 = Document::load(&bytes).unwrap();
            assert_eq!(
                d2.language().as_deref(),
                Some("de-DE"),
                "language survives save/load"
            );

            let mut d3 = Document::load(&bytes).unwrap();
            d3.set_language("");
            assert_eq!(d3.language(), None);
            assert_eq!(d3.materialize().language, None);
        }
        {
            // A document saved with no language must still open and read `None` (the `meta.language` register simply doesn't exist).
            let mut d = Document::new();
            let bytes = d.save();
            let d2 = Document::load(&bytes).unwrap();
            assert_eq!(d2.language(), None);
        }
    }

    #[test]
    fn blocks_text_and_marks_edit_in_order_and_materialize() {
        {
            let mut d = Document::new();
            let a = d.insert_block(None, "paragraph");
            let b = d.insert_block(Some(&a), "paragraph");
            let c = d.insert_block(Some(&a), "paragraph"); // between a and b
            assert_eq!(d.blocks_in_order(), vec![a.clone(), c.clone(), b]);
            assert_eq!(d.block_kind(&c).as_deref(), Some("paragraph"));
        }
        {
            let mut d = Document::new();
            let a = d.insert_block(None, "paragraph");
            let b = d.insert_block(Some(&a), "paragraph");
            let c = d.insert_block(Some(&b), "paragraph");
            d.remove_block(&b);
            assert_eq!(d.blocks_in_order(), vec![a.clone(), c.clone()]);
            d.move_block(&c, None); // to front
            assert_eq!(d.blocks_in_order(), vec![c, a]);
        }
        {
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "hello world");
            assert_eq!(d.block_text(&p).as_deref(), Some("hello world"));
            d.splice_text(&p, 5, 0, ",");
            assert_eq!(d.block_text(&p).as_deref(), Some("hello, world"));
            d.splice_text(&p, 0, 5, "HELLO");
            assert_eq!(d.block_text(&p).as_deref(), Some("HELLO, world"));
        }
        {
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "héllo");
            assert_eq!(d.block_text(&p).as_deref(), Some("héllo"));
        }
        {
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "hello world");
            d.add_mark(&p, 0, 5, "strong", true);
            let m = d.marks(&p);
            assert!(
                m.iter()
                    .any(|s| s.name == "strong" && s.start == 0 && s.end == 5),
                "expected strong[0,5], got {m:?}"
            );
        }
        {
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "hello world");
            d.add_mark(&p, 6, 11, "em", true); // "world"
            d.splice_text(&p, 0, 0, "say "); // shift right by 4
            let m = d.marks(&p);
            assert!(
                m.iter()
                    .any(|s| s.name == "em" && s.start == 10 && s.end == 15),
                "mark must track text shift, got {m:?}"
            );
        }
        {
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
        {
            let mut d = Document::new();
            let a = d.insert_block(None, "paragraph");
            d.splice_text(&a, 0, 0, "first");
            // Re-create a generator over the same doc + actor (as the editor does when binding to a loaded
            // document): a fresh IdGen at 0 would re-mint `a`'s id and overwrite it; the seeded one must not.
            let c = d.c.clone();
            let mut ids = crate::model::seeded_id_gen(&d.doc, &c);
            {
                let mut dm = DocMut {
                    doc: &mut d.doc,
                    c: &c,
                    ids: &mut ids,
                };
                let b = dm.insert_block(Some(&a), "paragraph");
                assert_ne!(a, b, "minted id must differ from the existing block");
                dm.splice_text(&b, 0, 0, "second");
            }
            let snap = d.materialize();
            assert_eq!(
                snap.blocks.len(),
                2,
                "must not overwrite the existing block"
            );
            assert_eq!(snap.blocks[0].text, "first");
            assert_eq!(snap.blocks[1].text, "second");
        }
    }

    #[test]
    fn notes_comments_and_tracked_changes_read_back_and_persist() {
        {
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "see ref");
            let nid = d.insert_footnote(&p, 4);
            let nb = d.insert_note_block(&nid, None, "paragraph").unwrap();
            d.splice_note_text(&nid, &nb, 0, 0, "footnote body");

            assert_eq!(d.note_ids(), vec![nid.clone()]);
            assert_eq!(d.note_kind(&nid), Some(crate::model::NoteKind::Footnote));
            assert_eq!(d.note_blocks(&nid)[0].text, "footnote body");
            let snap = d.materialize();
            assert_eq!(snap.notes.len(), 1);
            assert_eq!(snap.notes[0].id, nid);
            let bytes = d.save();
            let d2 = Document::load(&bytes).unwrap();
            assert_eq!(d2.note_blocks(&nid)[0].text, "footnote body");
            assert!(
                d2.materialize().blocks[0]
                    .marks
                    .iter()
                    .any(|m| m.name == crate::schema::MARK_NOTE_REF)
            );
        }
        {
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "review this paragraph");
            let cid = d.add_comment(&p, 7, 11, "Grace", "note: fix"); // over "this"

            assert_eq!(d.comment_ids(), vec![cid.clone()]);
            assert_eq!(d.comment_author(&cid).as_deref(), Some("Grace"));
            assert_eq!(d.comment_blocks(&cid)[0].text, "note: fix");
            assert_eq!(d.comment_range(&cid), Some((p.clone(), 7, 11)));
            let snap = d.materialize();
            assert_eq!(snap.comments.len(), 1);
            assert_eq!(snap.comments[0].id, cid);
            assert_eq!(snap.comments[0].range, Some((p.clone(), 7, 11)));
            let bytes = d.save();
            let d2 = Document::load(&bytes).unwrap();
            assert_eq!(d2.comment_blocks(&cid)[0].text, "note: fix");
            assert!(
                d2.materialize().blocks[0]
                    .marks
                    .iter()
                    .any(|m| m.name == crate::schema::MARK_COMMENT)
            );
        }
        {
            let mut d = Document::new();
            let p = d.insert_block(None, "paragraph");
            d.splice_text(&p, 0, 0, "the quick fox");
            let ins = d.track_insert(&p, 10, "brown ", "Grace", "2021-05-06");
            let del = d.track_delete(&p, 4, 10, "Hopper", "2021-07-08");

            let mut want = vec![ins.clone(), del.clone()];
            want.compact_sort();
            assert_eq!(d.change_ids(), want);
            assert_eq!(d.change_kind(&ins), Some(crate::snapshot::ChangeKind::Ins));
            assert_eq!(d.change_kind(&del), Some(crate::snapshot::ChangeKind::Del));
            assert_eq!(d.change_author(&ins).as_deref(), Some("Grace"));
            assert_eq!(d.change_date(&del).as_deref(), Some("2021-07-08"));
            assert_eq!(d.materialize().changes.len(), 2);

            let bytes = d.save();
            let d2 = Document::load(&bytes).unwrap();
            let snap = d2.materialize();
            assert_eq!(snap.changes.len(), 2, "changes survive save/load");
            assert!(
                snap.blocks[0]
                    .marks
                    .iter()
                    .any(|m| m.name == crate::schema::MARK_INS)
            );
            assert!(
                snap.blocks[0]
                    .marks
                    .iter()
                    .any(|m| m.name == crate::schema::MARK_DEL)
            );
        }
    }

    /// A document holding one paragraph with `text`, and a fork of it.
    fn forked(text: &str) -> (Document, Document, String) {
        let mut a = Document::new();
        let p = a.insert_block(None, "paragraph");
        a.splice_text(&p, 0, 0, text);
        let b = a.fork();
        (a, b, p)
    }

    /// Merge both ways and require the replicas to converge.
    fn converge(a: &mut Document, b: &mut Document, what: &str) {
        a.merge(b);
        b.merge(a);
        assert_eq!(
            a.materialize(),
            b.materialize(),
            "replicas converge: {what}"
        );
    }

    #[test]
    fn action_minted_ids_are_replay_stable_and_unique_per_action() {
        let mut d = Document::new();
        let a = d.insert_block(None, "paragraph");
        let c = d.c.clone();
        let mint = |doc: &mut automerge::AutoCommit, payload: &[u8]| {
            let mut replica = doc.fork();
            replica.set_actor(doc.get_actor().clone());
            let cx = tilo_resource::ActionCx::for_apply(&replica, payload, 0);
            let mut ids = crate::model::IdGen::for_action(cx);
            let mut dm = DocMut {
                doc: &mut replica,
                c: &c,
                ids: &mut ids,
            };
            [
                dm.insert_block(Some(&a), "paragraph"),
                dm.insert_block(Some(&a), "paragraph"),
            ]
        };
        let first = mint(&mut d.doc, b"split");
        assert_eq!(
            first,
            mint(&mut d.doc, b"split"),
            "replaying mints the same ids"
        );
        assert_ne!(first[0], first[1]);
        assert_ne!(first, mint(&mut d.doc, b"another action"));
        assert!(!first.contains(&a));
    }

    /// Two replicas edit the same document concurrently; after merging both
    /// ways they converge with both sides' edits present (CRDT-native).
    #[test]
    fn replicas_converge_with_both_sides_concurrent_edits() {
        let (mut a, mut b, p) = forked("base");
        a.splice_text(&p, 4, 0, "-A");
        let q = b.insert_block(Some(&p), "paragraph");
        b.splice_text(&q, 0, 0, "B-block");
        converge(&mut a, &mut b, "disjoint edits");
        assert_eq!(a.blocks_in_order().len(), 2);

        let (mut a, mut b, p) = forked("base");
        let na = a.insert_footnote(&p, 4);
        let nab = a.insert_note_block(&na, None, "paragraph").unwrap();
        a.splice_note_text(&na, &nab, 0, 0, "from A");
        let nb = b.insert_endnote(&p, 0);
        let nbb = b.insert_note_block(&nb, None, "paragraph").unwrap();
        b.splice_note_text(&nb, &nbb, 0, 0, "from B");
        converge(&mut a, &mut b, "notes");
        assert_eq!(
            a.materialize().notes.len(),
            2,
            "both notes present after merge"
        );

        let (mut a, mut b, p) = forked("base text here");
        a.add_comment(&p, 0, 4, "A", "from A"); // over "base"
        b.add_comment(&p, 5, 9, "B", "from B"); // over "text"
        converge(&mut a, &mut b, "comments");
        assert_eq!(
            a.materialize().comments.len(),
            2,
            "both comments present after merge"
        );

        // Concurrent replies to one comment do not replace each other.
        let mut a = Document::new();
        let p = a.insert_block(None, "paragraph");
        a.splice_text(&p, 0, 0, "shared text");
        let comment = a.add_comment(&p, 0, 6, "Ada", "Please review");
        let mut b = a.fork();
        let ar = a
            .add_comment_reply(&comment, "Bea", "2026-09-07", "Looks good")
            .expect("comment exists on A");
        let br = b
            .add_comment_reply(&comment, "Cy", "2026-09-08", "One suggestion")
            .expect("comment exists on B");
        converge(&mut a, &mut b, "comment thread");
        let replies = a.comment_replies(&comment);
        assert_eq!(replies.len(), 2);
        assert!(replies.iter().any(|reply| reply.id == ar));
        assert!(replies.iter().any(|reply| reply.id == br));

        let (mut a, mut b, p) = forked("shared base text");
        a.track_delete(&p, 0, 6, "A", ""); // "shared"
        b.track_insert(&p, 16, "!", "B", "");
        converge(&mut a, &mut b, "tracked changes");
        assert_eq!(
            a.materialize().changes.len(),
            2,
            "both changes present after merge"
        );
    }
}
