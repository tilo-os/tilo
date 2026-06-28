//! Borrowed-view model operations over a `&mut AutoCommit`, so a document can
//! live in any host's automerge doc (e.g. `ui::State.doc`) — not only inside an
//! owned [`crate::Document`]. The owned `Document` is a thin wrapper that
//! delegates here.

use crate::document::{DocError, MarkSpan};
use crate::schema::*;
use crate::snapshot::{AttrVal, BlockView, DocSnapshot};
use automerge::marks::{ExpandMark, Mark};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use std::collections::BTreeMap;

/// Resolved object ids for the canonical container maps under a document root.
#[derive(Clone, Debug)]
pub struct Containers {
    pub blocks: ObjId,
    pub styles: ObjId,
    pub preserved: ObjId,
    pub meta: ObjId,
}

/// Per-replica, collision-free block id generator: `{actor_hex}.{counter}`.
pub struct IdGen {
    actor_hex: String,
    next: u64,
}

impl IdGen {
    pub fn new(actor_hex: String) -> Self {
        Self { actor_hex, next: 0 }
    }
    /// Start the counter at `next` — used to seed past ids that already exist in
    /// a loaded/opened document (otherwise a fresh generator re-mints existing
    /// ids and overwrites those blocks).
    pub fn with_counter(actor_hex: String, next: u64) -> Self {
        Self { actor_hex, next }
    }
    pub fn mint(&mut self) -> String {
        let id = format!("{}.{}", self.actor_hex, self.next);
        self.next += 1;
        id
    }
}

/// The next free id counter for `actor_hex`, scanning the existing block ids so a
/// generator created over an already-populated document won't collide.
pub fn next_counter_for(doc: &AutoCommit, c: &Containers, actor_hex: &str) -> u64 {
    let prefix = format!("{actor_hex}.");
    let mut next = 0u64;
    for id in blocks_in_order(doc, c) {
        if let Some(rest) = id.strip_prefix(&prefix) {
            if let Ok(n) = rest.parse::<u64>() {
                next = next.max(n + 1);
            }
        }
    }
    next
}

/// An [`IdGen`] for `doc`'s current actor, seeded past existing block ids.
pub fn seeded_id_gen(doc: &AutoCommit, c: &Containers) -> IdGen {
    let actor = doc.get_actor().to_hex_string();
    let next = next_counter_for(doc, c, &actor);
    IdGen::with_counter(actor, next)
}

/// Write schema scalars + create the four container maps under `root`.
pub fn init(doc: &mut AutoCommit, root: &ObjId) -> Containers {
    doc.put(root, KEY_SCHEMA, SCHEMA_ID).unwrap();
    doc.put(root, KEY_SCHEMA_VERSION, SCHEMA_VERSION).unwrap();
    let blocks = doc.put_object(root, KEY_BLOCKS, ObjType::Map).unwrap();
    let styles = doc.put_object(root, KEY_STYLES, ObjType::Map).unwrap();
    let preserved = doc.put_object(root, KEY_PRESERVED, ObjType::Map).unwrap();
    let meta = doc.put_object(root, KEY_META, ObjType::Map).unwrap();
    Containers { blocks, styles, preserved, meta }
}

/// Resolve existing containers under `root`.
pub fn open(doc: &AutoCommit, root: &ObjId) -> Result<Containers, DocError> {
    Ok(Containers {
        blocks: map_at(doc, root, KEY_BLOCKS)?,
        styles: map_at(doc, root, KEY_STYLES)?,
        preserved: map_at(doc, root, KEY_PRESERVED)?,
        meta: map_at(doc, root, KEY_META)?,
    })
}

fn map_at(doc: &AutoCommit, root: &ObjId, key: &str) -> Result<ObjId, DocError> {
    match doc.get(root, key)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Err(DocError::Invalid(format!("root.{key} is not a map"))),
    }
}

// ---- reads -----------------------------------------------------------------

fn block_obj(doc: &AutoCommit, container: &ObjId, id: &str) -> Option<ObjId> {
    match doc.get(container, id) {
        Ok(Some((Value::Object(ObjType::Map), oid))) => Some(oid),
        _ => None,
    }
}

fn block_order(doc: &AutoCommit, container: &ObjId, id: &str) -> Option<Vec<u8>> {
    let obj = block_obj(doc, container, id)?;
    match doc.get(&obj, BLOCK_ORDER) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Bytes(b) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn ordered_keys(doc: &AutoCommit, container: &ObjId) -> Vec<String> {
    let mut v: Vec<(Vec<u8>, String)> = doc
        .keys(container)
        .filter_map(|id| block_order(doc, container, &id).map(|ord| (ord, id)))
        .collect();
    v.sort_by(|a, b| crate::order::cmp_keys(&a.0, &b.0).then(a.1.cmp(&b.1)));
    v.into_iter().map(|(_, id)| id).collect()
}

pub fn blocks_in_order(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    ordered_keys(doc, &c.blocks)
}

pub fn block_kind(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let obj = block_obj(doc, &c.blocks, id)?;
    match doc.get(&obj, BLOCK_KIND) {
        Ok(Some((v, _))) => v.to_str().map(|s| s.to_string()),
        _ => None,
    }
}

pub fn block_text(doc: &AutoCommit, c: &Containers, id: &str) -> Option<String> {
    let obj = block_obj(doc, &c.blocks, id)?;
    match doc.get(&obj, BLOCK_TEXT) {
        Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).ok(),
        _ => None,
    }
}

fn marks_on(doc: &AutoCommit, block_obj: &ObjId) -> Vec<MarkSpan> {
    let tid = match doc.get(block_obj, BLOCK_TEXT) {
        Ok(Some((Value::Object(ObjType::Text), t))) => t,
        _ => return vec![],
    };
    let mut spans: Vec<MarkSpan> = doc
        .marks(&tid)
        .map(|ms| {
            ms.into_iter()
                .map(|m| MarkSpan { start: m.start, end: m.end, name: m.name().to_string(), value: m.value().into() })
                .collect()
        })
        .unwrap_or_default();
    // Canonical order so two replicas that merged in different orders produce
    // byte-identical materializations (mark *set* is order-independent). The
    // value is part of the key so concurrent *valued* marks (fontSize/link/color)
    // that resolve to distinct spans still sort deterministically.
    spans.sort_by(|a, b| (a.start, a.end, &a.name, &a.value).cmp(&(b.start, b.end, &b.name, &b.value)));
    spans
}

pub fn marks(doc: &AutoCommit, c: &Containers, id: &str) -> Vec<MarkSpan> {
    match block_obj(doc, &c.blocks, id) {
        Some(obj) => marks_on(doc, &obj),
        None => vec![],
    }
}

/// Whether a mark named `name` fully covers `[start, end)` of block `id`.
pub fn has_mark_over(doc: &AutoCommit, c: &Containers, id: &str, start: usize, end: usize, name: &str) -> bool {
    marks(doc, c, id)
        .iter()
        .any(|m| m.name == name && m.start <= start && m.end >= end)
}

/// All preserved blobs (the lossless preservation layer — verbatim bytes a codec
/// stashed for content it doesn't model), as `(key, bytes)`.
pub fn preserved_parts(doc: &AutoCommit, c: &Containers) -> Vec<(String, Vec<u8>)> {
    doc.keys(&c.preserved)
        .filter_map(|k| match doc.get(&c.preserved, &k) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Bytes(b) => Some((k, b.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn read_attrs(doc: &AutoCommit, block_obj: &ObjId) -> BTreeMap<String, AttrVal> {
    let attrs_obj = match doc.get(block_obj, BLOCK_ATTRS) {
        Ok(Some((Value::Object(ObjType::Map), a))) => a,
        _ => return BTreeMap::new(),
    };
    let mut out = BTreeMap::new();
    for key in doc.keys(&attrs_obj) {
        if let Ok(Some((Value::Scalar(s), _))) = doc.get(&attrs_obj, &key) {
            let val = match s.as_ref() {
                ScalarValue::Int(i) => AttrVal::Int(*i),
                ScalarValue::Uint(u) => AttrVal::Int(*u as i64),
                ScalarValue::Boolean(b) => AttrVal::Bool(*b),
                ScalarValue::Str(st) => AttrVal::Str(st.to_string()),
                _ => continue,
            };
            out.insert(key, val);
        }
    }
    out
}

pub fn materialize(doc: &AutoCommit, c: &Containers) -> DocSnapshot {
    let blocks = blocks_in_order(doc, c)
        .into_iter()
        .map(|id| materialize_block(doc, &c.blocks, &id))
        .collect();
    DocSnapshot { schema: SCHEMA_ID.to_string(), schema_version: SCHEMA_VERSION, blocks }
}

fn materialize_block(doc: &AutoCommit, container: &ObjId, id: &str) -> BlockView {
    let obj = match doc.get(container, id) {
        Ok(Some((Value::Object(ObjType::Map), o))) => o,
        _ => {
            return BlockView {
                id: id.into(),
                kind: String::new(),
                text: String::new(),
                marks: vec![],
                attrs: BTreeMap::new(),
                children: vec![],
            }
        }
    };
    let kind = match doc.get(&obj, BLOCK_KIND) {
        Ok(Some((v, _))) => v.to_str().unwrap_or("").to_string(),
        _ => String::new(),
    };
    let text = match doc.get(&obj, BLOCK_TEXT) {
        Ok(Some((Value::Object(ObjType::Text), tid))) => doc.text(&tid).unwrap_or_default(),
        _ => String::new(),
    };
    let marks = marks_on(doc, &obj);
    let attrs = read_attrs(doc, &obj);
    let children = match doc.get(&obj, BLOCK_CHILDREN) {
        Ok(Some((Value::Object(ObjType::Map), cid))) => ordered_keys(doc, &cid)
            .into_iter()
            .map(|cc| materialize_block(doc, &cid, &cc))
            .collect(),
        _ => vec![],
    };
    BlockView { id: id.into(), kind, text, marks, attrs, children }
}

// ---- borrowed mutable view -------------------------------------------------

/// A mutable view of a canonical document living in a borrowed `AutoCommit`.
/// Used by the editor (over `ui::State.doc`) and by codecs.
pub struct DocMut<'a> {
    pub doc: &'a mut AutoCommit,
    pub c: &'a Containers,
    pub ids: &'a mut IdGen,
}

/// A read-only view, used by codec `export`.
pub struct DocRef<'a> {
    pub doc: &'a AutoCommit,
    pub c: &'a Containers,
}

impl<'a> DocRef<'a> {
    pub fn materialize(&self) -> DocSnapshot {
        materialize(self.doc, self.c)
    }
}

impl<'a> DocMut<'a> {
    fn order_after(&self, after: Option<&str>) -> Vec<u8> {
        let doc: &AutoCommit = &*self.doc;
        let ordered = blocks_in_order(doc, self.c);
        let pos = match after {
            None => 0,
            Some(id) => ordered
                .iter()
                .position(|x| x == id)
                .map(|i| i + 1)
                .unwrap_or(ordered.len()),
        };
        let lo = if pos == 0 { None } else { block_order(doc, &self.c.blocks, &ordered[pos - 1]) };
        let hi = if pos >= ordered.len() { None } else { block_order(doc, &self.c.blocks, &ordered[pos]) };
        crate::order::key_between(lo.as_deref(), hi.as_deref())
    }

    // NOTE: DocMut does NOT commit the automerge transaction — the host owns the
    // transaction boundary. The owned `Document` commits per op (for fine-grained
    // history); a host like `ui::State` commits once per user action (so its
    // snapshot-based undo records each action). Reads always observe pending ops.

    pub fn insert_block(&mut self, after: Option<&str>, kind: &str) -> String {
        let key = self.order_after(after);
        let id = self.ids.mint();
        let obj = self.doc.put_object(&self.c.blocks, id.as_str(), ObjType::Map).unwrap();
        self.doc.put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key)).unwrap();
        self.doc.put(&obj, BLOCK_KIND, kind).unwrap();
        self.doc.put_object(&obj, BLOCK_ATTRS, ObjType::Map).unwrap();
        id
    }

    pub fn remove_block(&mut self, id: &str) {
        let _ = self.doc.delete(&self.c.blocks, id);
    }

    pub fn move_block(&mut self, id: &str, after: Option<&str>) {
        let key = self.order_after(after);
        if let Some(obj) = block_obj(self.doc, &self.c.blocks, id) {
            self.doc.put(&obj, BLOCK_ORDER, ScalarValue::Bytes(key)).unwrap();
        }
    }

    fn text_obj(&mut self, block_id: &str) -> Option<ObjId> {
        let obj = block_obj(self.doc, &self.c.blocks, block_id)?;
        let existing = match self.doc.get(&obj, BLOCK_TEXT) {
            Ok(Some((Value::Object(ObjType::Text), tid))) => Some(tid),
            _ => None,
        };
        if let Some(tid) = existing {
            return Some(tid);
        }
        self.doc.put_object(&obj, BLOCK_TEXT, ObjType::Text).ok()
    }

    pub fn splice_text(&mut self, block_id: &str, idx: usize, del: isize, ins: &str) {
        if let Some(tid) = self.text_obj(block_id) {
            let _ = self.doc.splice_text(&tid, idx, del, ins);
        }
    }

    pub fn add_mark(&mut self, block_id: &str, start: usize, end: usize, name: &str, value: impl Into<ScalarValue>) {
        if let Some(tid) = self.text_obj(block_id) {
            let mark = Mark::new(name.to_string(), value, start, end);
            let _ = self.doc.mark(&tid, mark, ExpandMark::After);
        }
    }

    /// Set a scalar attribute (e.g. heading `level`, list `ordered`) in the
    /// block's `attrs` map.
    pub fn set_attr(&mut self, block_id: &str, key: &str, value: impl Into<ScalarValue>) {
        let Some(obj) = block_obj(self.doc, &self.c.blocks, block_id) else {
            return;
        };
        let attrs = match self.doc.get(&obj, BLOCK_ATTRS) {
            Ok(Some((Value::Object(ObjType::Map), a))) => a,
            _ => return,
        };
        let _ = self.doc.put(&attrs, key, value);
    }

    /// Change a block's kind (e.g. `paragraph` ↔ `heading` ↔ `list_item`).
    pub fn set_kind(&mut self, block_id: &str, kind: &str) {
        if let Some(obj) = block_obj(self.doc, &self.c.blocks, block_id) {
            let _ = self.doc.put(&obj, BLOCK_KIND, kind);
        }
    }

    /// Remove a mark named `name` over `[start, end)` of the block's text.
    pub fn remove_mark(&mut self, block_id: &str, start: usize, end: usize, name: &str) {
        if let Some(tid) = self.text_obj(block_id) {
            let _ = self.doc.unmark(&tid, name, start, end, ExpandMark::After);
        }
    }

    /// Stash a verbatim blob in the preservation layer (e.g. an unmodeled ODF
    /// part), keyed by `key`. Re-emitted unchanged on export.
    pub fn set_preserved(&mut self, key: &str, bytes: &[u8]) {
        let _ = self.doc.put(&self.c.preserved, key, ScalarValue::Bytes(bytes.to_vec()));
    }
}
