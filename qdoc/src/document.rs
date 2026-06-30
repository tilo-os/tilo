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
