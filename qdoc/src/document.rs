//! `Document`: a handle over an automerge `AutoCommit` holding the canonical
//! rich-text model.

use crate::schema::*;
use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value, ROOT};

#[derive(thiserror::Error, Debug)]
pub enum DocError {
    #[error("automerge error: {0}")]
    Automerge(#[from] automerge::AutomergeError),
    #[error("load error: {0}")]
    Load(String),
    #[error("invalid document: {0}")]
    Invalid(String),
}

/// One inline formatting span as seen from the read side.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MarkSpan {
    pub start: usize,
    pub end: usize,
    pub name: String,
}

pub struct Document {
    pub(crate) doc: AutoCommit,
    pub(crate) blocks: ObjId,
    #[allow(dead_code)]
    pub(crate) styles: ObjId,
    #[allow(dead_code)]
    pub(crate) preserved: ObjId,
    #[allow(dead_code)]
    pub(crate) meta: ObjId,
    next_local: u64,
}

impl Document {
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        doc.put(&ROOT, KEY_SCHEMA, SCHEMA_ID).unwrap();
        doc.put(&ROOT, KEY_SCHEMA_VERSION, SCHEMA_VERSION).unwrap();
        let blocks = doc.put_object(&ROOT, KEY_BLOCKS, ObjType::Map).unwrap();
        let styles = doc.put_object(&ROOT, KEY_STYLES, ObjType::Map).unwrap();
        let preserved = doc.put_object(&ROOT, KEY_PRESERVED, ObjType::Map).unwrap();
        let meta = doc.put_object(&ROOT, KEY_META, ObjType::Map).unwrap();
        doc.commit();
        Self { doc, blocks, styles, preserved, meta, next_local: 0 }
    }

    pub fn load(bytes: &[u8]) -> Result<Self, DocError> {
        let doc = AutoCommit::load(bytes).map_err(|e| DocError::Load(e.to_string()))?;
        let blocks = obj_at(&doc, KEY_BLOCKS)?;
        let styles = obj_at(&doc, KEY_STYLES)?;
        let preserved = obj_at(&doc, KEY_PRESERVED)?;
        let meta = obj_at(&doc, KEY_META)?;
        Ok(Self { doc, blocks, styles, preserved, meta, next_local: 0 })
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    pub fn actor_hex(&self) -> String {
        self.doc.get_actor().to_hex_string()
    }

    /// Mint a globally-unique block id: `{actor_hex}.{counter}`.
    pub(crate) fn mint_id(&mut self) -> String {
        let id = format!("{}.{}", self.actor_hex(), self.next_local);
        self.next_local += 1;
        id
    }

    // ---- blocks ------------------------------------------------------------

    pub fn blocks_in_order(&self) -> Vec<String> {
        let mut v: Vec<(Vec<u8>, String)> = self
            .doc
            .keys(&self.blocks)
            .filter_map(|id| self.block_order(&id).map(|ord| (ord, id)))
            .collect();
        v.sort_by(|a, b| crate::order::cmp_keys(&a.0, &b.0).then(a.1.cmp(&b.1)));
        v.into_iter().map(|(_, id)| id).collect()
    }

    fn block_obj(&self, id: &str) -> Option<ObjId> {
        match self.doc.get(&self.blocks, id) {
            Ok(Some((Value::Object(ObjType::Map), oid))) => Some(oid),
            _ => None,
        }
    }

    fn block_order(&self, id: &str) -> Option<Vec<u8>> {
        let obj = self.block_obj(id)?;
        match self.doc.get(&obj, BLOCK_ORDER) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Bytes(b) => Some(b.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn block_kind(&self, id: &str) -> Option<String> {
        let obj = self.block_obj(id)?;
        match self.doc.get(&obj, BLOCK_KIND) {
            Ok(Some((v, _))) => v.to_str().map(|s| s.to_string()),
            _ => None,
        }
    }

    /// Key sorting immediately after `after` (or at the very front if None),
    /// and before whatever currently follows it.
    fn order_after(&self, after: Option<&str>) -> Vec<u8> {
        let ordered = self.blocks_in_order();
        let pos = match after {
            None => 0,
            Some(id) => ordered
                .iter()
                .position(|x| x == id)
                .map(|i| i + 1)
                .unwrap_or(ordered.len()),
        };
        let lo = if pos == 0 { None } else { self.block_order(&ordered[pos - 1]) };
        let hi = if pos >= ordered.len() { None } else { self.block_order(&ordered[pos]) };
        crate::order::key_between(lo.as_deref(), hi.as_deref())
    }

    pub fn insert_block(&mut self, after: Option<&str>, kind: &str) -> String {
        let key = self.order_after(after);
        let id = self.mint_id();
        let obj = self.doc.put_object(&self.blocks, id.as_str(), ObjType::Map).unwrap();
        self.doc.put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key)).unwrap();
        self.doc.put(&obj, BLOCK_KIND, kind).unwrap();
        self.doc.put_object(&obj, BLOCK_ATTRS, ObjType::Map).unwrap();
        self.doc.commit();
        id
    }

    pub fn remove_block(&mut self, id: &str) {
        let _ = self.doc.delete(&self.blocks, id);
        self.doc.commit();
    }

    pub fn move_block(&mut self, id: &str, after: Option<&str>) {
        let key = self.order_after(after);
        if let Some(obj) = self.block_obj(id) {
            self.doc.put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key)).unwrap();
            self.doc.commit();
        }
    }

    // ---- text --------------------------------------------------------------

    /// Resolve the block's `Text` object, creating it on first edit.
    fn text_obj(&mut self, block_id: &str) -> Option<ObjId> {
        let obj = self.block_obj(block_id)?;
        // Scope the read borrow so the mutable `put_object` below is allowed.
        let existing = match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => Some(tid),
            _ => None,
        };
        if let Some(tid) = existing {
            return Some(tid);
        }
        self.doc.put_object(&obj, BLOCK_TEXT, ObjType::Text).ok()
    }

    pub fn splice_text(&mut self, block_id: &str, char_idx: usize, del: isize, ins: &str) {
        if let Some(tid) = self.text_obj(block_id) {
            let _ = self.doc.splice_text(&tid, char_idx, del, ins);
            self.doc.commit();
        }
    }

    pub fn block_text(&self, block_id: &str) -> Option<String> {
        let obj = self.block_obj(block_id)?;
        match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => self.doc.text(&tid).ok(),
            _ => None,
        }
    }

    // ---- marks -------------------------------------------------------------

    pub fn add_mark(&mut self, block_id: &str, start: usize, end: usize, name: &str, value: bool) {
        if let Some(tid) = self.text_obj(block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
            self.doc.commit();
        }
    }

    pub fn marks(&self, block_id: &str) -> Vec<MarkSpan> {
        let Some(obj) = self.block_obj(block_id) else {
            return vec![];
        };
        let tid = match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => tid,
            _ => return vec![],
        };
        self.doc
            .marks(&tid)
            .map(|ms| {
                ms.into_iter()
                    .map(|m| MarkSpan { start: m.start, end: m.end, name: m.name().to_string() })
                    .collect()
            })
            .unwrap_or_default()
    }

    // ---- materialize -------------------------------------------------------

    pub fn materialize(&self) -> crate::snapshot::DocSnapshot {
        let blocks = self
            .blocks_in_order()
            .into_iter()
            .map(|id| self.materialize_block(&id, &self.blocks))
            .collect();
        crate::snapshot::DocSnapshot {
            schema: SCHEMA_ID.to_string(),
            schema_version: SCHEMA_VERSION,
            blocks,
        }
    }

    fn materialize_block(&self, id: &str, parent: &ObjId) -> crate::snapshot::BlockView {
        let obj = match self.doc.get(parent, id) {
            Ok(Some((Value::Object(ObjType::Map), oid))) => oid,
            _ => {
                return crate::snapshot::BlockView {
                    id: id.into(),
                    kind: String::new(),
                    text: String::new(),
                    marks: vec![],
                    children: vec![],
                }
            }
        };
        let kind = match self.doc.get(&obj, BLOCK_KIND) {
            Ok(Some((v, _))) => v.to_str().unwrap_or("").to_string(),
            _ => String::new(),
        };
        let text = match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => self.doc.text(&tid).unwrap_or_default(),
            _ => String::new(),
        };
        let marks = self.marks(id);
        let children = match self.doc.get(&obj, BLOCK_CHILDREN) {
            Ok(Some((Value::Object(ObjType::Map), cid))) => self
                .children_in_order(&cid)
                .into_iter()
                .map(|c| self.materialize_block(&c, &cid))
                .collect(),
            _ => vec![],
        };
        crate::snapshot::BlockView { id: id.into(), kind, text, marks, children }
    }

    fn children_in_order(&self, container: &ObjId) -> Vec<String> {
        let mut v: Vec<(Vec<u8>, String)> = self
            .doc
            .keys(container)
            .filter_map(|id| {
                let cobj = match self.doc.get(container, &id) {
                    Ok(Some((Value::Object(ObjType::Map), o))) => o,
                    _ => return None,
                };
                let ord = match self.doc.get(&cobj, BLOCK_ORDER) {
                    Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                        ScalarValue::Bytes(b) => b.clone(),
                        _ => return None,
                    },
                    _ => return None,
                };
                Some((ord, id))
            })
            .collect();
        v.sort_by(|a, b| crate::order::cmp_keys(&a.0, &b.0).then(a.1.cmp(&b.1)));
        v.into_iter().map(|(_, id)| id).collect()
    }

    // ---- replication -------------------------------------------------------

    /// Independent replica with a fresh actor id (distinct edit lineage).
    pub fn fork(&mut self) -> Document {
        let bytes = self.save();
        let mut d = Document::load(&bytes).expect("fork load");
        d.doc.set_actor(automerge::ActorId::random());
        d
    }

    /// Merge another replica's changes into this one.
    pub fn merge(&mut self, other: &mut Document) {
        let _ = self.doc.merge(&mut other.doc);
        self.doc.commit();
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

fn obj_at(doc: &AutoCommit, key: &str) -> Result<ObjId, DocError> {
    match doc.get(&ROOT, key)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Err(DocError::Invalid(format!("root.{key} is not a map"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_doc_has_root_layout() {
        let d = Document::new();
        let (v, _) = d.doc.get(&ROOT, KEY_SCHEMA).unwrap().unwrap();
        assert_eq!(v.to_str(), Some(SCHEMA_ID));
        let (vv, _) = d.doc.get(&ROOT, KEY_SCHEMA_VERSION).unwrap().unwrap();
        assert_eq!(vv.to_i64(), Some(SCHEMA_VERSION));
        for k in [KEY_BLOCKS, KEY_STYLES, KEY_PRESERVED, KEY_META] {
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
