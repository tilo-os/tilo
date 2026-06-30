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
    /// The document root these containers live under. Needed so the blobs map can
    /// be created lazily for documents opened before the blobs map existed (the
    /// root is not always `automerge::ROOT` — a host may nest the model).
    pub root: ObjId,
    pub blocks: ObjId,
    pub styles: ObjId,
    pub preserved: ObjId,
    pub meta: ObjId,
    /// Content-addressed binary store (blake3 hex → `Bytes`). `None` for documents
    /// saved under an older schema that predates the blobs map; created lazily on
    /// the first [`DocMut::put_blob`].
    pub blobs: Option<ObjId>,
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

/// Write schema scalars + create the container maps under `root`.
pub fn init(doc: &mut AutoCommit, root: &ObjId) -> Containers {
    doc.put(root, KEY_SCHEMA, SCHEMA_ID).unwrap();
    doc.put(root, KEY_SCHEMA_VERSION, SCHEMA_VERSION).unwrap();
    let blocks = doc.put_object(root, KEY_BLOCKS, ObjType::Map).unwrap();
    let styles = doc.put_object(root, KEY_STYLES, ObjType::Map).unwrap();
    let preserved = doc.put_object(root, KEY_PRESERVED, ObjType::Map).unwrap();
    let meta = doc.put_object(root, KEY_META, ObjType::Map).unwrap();
    let blobs = doc.put_object(root, KEY_BLOBS, ObjType::Map).unwrap();
    Containers { root: root.clone(), blocks, styles, preserved, meta, blobs: Some(blobs) }
}

/// Resolve existing containers under `root`. The blobs map is optional so
/// documents written before it existed still open (it is then created lazily).
pub fn open(doc: &AutoCommit, root: &ObjId) -> Result<Containers, DocError> {
    Ok(Containers {
        root: root.clone(),
        blocks: map_at(doc, root, KEY_BLOCKS)?,
        styles: map_at(doc, root, KEY_STYLES)?,
        preserved: map_at(doc, root, KEY_PRESERVED)?,
        meta: map_at(doc, root, KEY_META)?,
        blobs: opt_map_at(doc, root, KEY_BLOBS),
    })
}

fn map_at(doc: &AutoCommit, root: &ObjId, key: &str) -> Result<ObjId, DocError> {
    match doc.get(root, key)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Err(DocError::Invalid(format!("root.{key} is not a map"))),
    }
}

/// Resolve an optional map (returns `None` if absent rather than erroring).
fn opt_map_at(doc: &AutoCommit, root: &ObjId, key: &str) -> Option<ObjId> {
    match doc.get(root, key) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

/// Resolve the blobs map object, preferring the cached id and falling back to a
/// fresh root lookup (so a lazily-created map is still found via reads).
fn blobs_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    if let Some(b) = &c.blobs {
        return Some(b.clone());
    }
    opt_map_at(doc, &c.root, KEY_BLOBS)
}

/// Fetch a content-addressed blob by its blake3 hex hash.
pub fn blob(doc: &AutoCommit, c: &Containers, hash: &str) -> Option<Vec<u8>> {
    let blobs = blobs_obj(doc, c)?;
    match doc.get(&blobs, hash) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Bytes(b) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// All content-addressed blobs (`blake3_hex` → bytes).
pub fn blobs(doc: &AutoCommit, c: &Containers) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    if let Some(blobs) = blobs_obj(doc, c) {
        for k in doc.keys(&blobs) {
            if let Ok(Some((Value::Scalar(s), _))) = doc.get(&blobs, &k) {
                if let ScalarValue::Bytes(b) = s.as_ref() {
                    out.insert(k, b.clone());
                }
            }
        }
    }
    out
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

/// Read a document-level meta string (e.g. the serialized stylesheet).
pub fn meta_str(doc: &AutoCommit, c: &Containers, key: &str) -> Option<String> {
    match doc.get(&c.meta, key) {
        Ok(Some((v, _))) => v.to_str().map(|s| s.to_string()),
        _ => None,
    }
}

/// Resolve the document-properties sub-map under `meta` (`meta.props`), if it
/// exists. Created lazily on the first [`DocMut::set_doc_prop`], so documents
/// authored before the props map existed simply read as having no properties.
fn props_obj(doc: &AutoCommit, c: &Containers) -> Option<ObjId> {
    match doc.get(&c.meta, KEY_PROPS) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

/// Read a single document property (`title`/`author`/… — see [`DOC_PROP_KEYS`]),
/// or any other string property name a codec stashed. `None` if unset.
pub fn doc_prop(doc: &AutoCommit, c: &Containers, name: &str) -> Option<String> {
    let props = props_obj(doc, c)?;
    match doc.get(&props, name) {
        Ok(Some((v, _))) => v.to_str().map(|s| s.to_string()),
        _ => None,
    }
}

/// All document properties (`name → value`) as a sorted map. Empty for documents
/// with no properties set.
pub fn doc_props(doc: &AutoCommit, c: &Containers) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(props) = props_obj(doc, c) {
        for k in doc.keys(&props) {
            if let Ok(Some((v, _))) = doc.get(&props, &k) {
                if let Some(s) = v.to_str() {
                    out.insert(k, s.to_string());
                }
            }
        }
    }
    out
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
                ScalarValue::F64(f) => AttrVal::Float(*f),
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
    DocSnapshot {
        schema: SCHEMA_ID.to_string(),
        schema_version: SCHEMA_VERSION,
        blocks,
        blobs: blobs(doc, c),
        styles: all_styles(doc, c),
        properties: doc_props(doc, c),
    }
}

// ---- styles container ------------------------------------------------------

/// Read a style sub-map's scalar properties into an [`AttrVal`] map.
fn read_style_props(doc: &AutoCommit, style_obj: &ObjId) -> BTreeMap<String, AttrVal> {
    let mut out = BTreeMap::new();
    for key in doc.keys(style_obj) {
        if let Ok(Some((Value::Scalar(s), _))) = doc.get(style_obj, &key) {
            let val = match s.as_ref() {
                ScalarValue::Int(i) => AttrVal::Int(*i),
                ScalarValue::Uint(u) => AttrVal::Int(*u as i64),
                ScalarValue::Boolean(b) => AttrVal::Bool(*b),
                ScalarValue::Str(st) => AttrVal::Str(st.to_string()),
                ScalarValue::F64(f) => AttrVal::Float(*f),
                _ => continue,
            };
            out.insert(key, val);
        }
    }
    out
}

/// Style ids defined in the document's styles container (e.g. `body`,
/// `heading1`, or an imported ODF named-style name).
pub fn style_ids(doc: &AutoCommit, c: &Containers) -> Vec<String> {
    doc.keys(&c.styles)
        .filter(|id| matches!(doc.get(&c.styles, id), Ok(Some((Value::Object(ObjType::Map), _)))))
        .collect()
}

/// The scalar properties of a single style (`name`/`font`/`size`/… → value), or
/// an empty map if the style id is unknown.
pub fn style_props(doc: &AutoCommit, c: &Containers, id: &str) -> BTreeMap<String, AttrVal> {
    match doc.get(&c.styles, id) {
        Ok(Some((Value::Object(ObjType::Map), obj))) => read_style_props(doc, &obj),
        _ => BTreeMap::new(),
    }
}

/// Every style in the container (`style_id → (property → value)`).
pub fn all_styles(doc: &AutoCommit, c: &Containers) -> BTreeMap<String, BTreeMap<String, AttrVal>> {
    let mut out = BTreeMap::new();
    for id in style_ids(doc, c) {
        let props = style_props(doc, c, &id);
        out.insert(id, props);
    }
    out
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

    /// Set a document-level meta string (e.g. the serialized stylesheet).
    pub fn set_meta_str(&mut self, key: &str, value: &str) {
        let _ = self.doc.put(&self.c.meta, key, value);
    }

    /// Resolve-or-create the `meta.props` document-properties sub-map. Created
    /// lazily so documents authored before it existed migrate transparently.
    fn props_map(&mut self) -> ObjId {
        match self.doc.get(&self.c.meta, KEY_PROPS) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => self.doc.put_object(&self.c.meta, KEY_PROPS, ObjType::Map).unwrap(),
        }
    }

    /// Set a document property (`title`/`author`/`subject`/`keywords`/`created`/
    /// `modified`/`generator` — see [`DOC_PROP_KEYS`]). Each property is its own
    /// scalar register in the `meta.props` map, so two replicas editing different
    /// properties field-level merge rather than clobbering each other. Round-trips
    /// through `.odt`/`.docx` metadata parts.
    pub fn set_doc_prop(&mut self, name: &str, value: &str) {
        let props = self.props_map();
        let _ = self.doc.put(&props, name, value);
    }

    /// Upsert a named style into the styles container. Each property is written
    /// as its own scalar register *into the existing sub-map* (not a fresh map),
    /// so two replicas editing different properties of the same style field-level
    /// merge rather than clobbering each other.
    pub fn set_style(&mut self, id: &str, props: &BTreeMap<String, AttrVal>) {
        let obj = match self.doc.get(&self.c.styles, id) {
            Ok(Some((Value::Object(ObjType::Map), o))) => o,
            _ => self.doc.put_object(&self.c.styles, id, ObjType::Map).unwrap(),
        };
        for (k, v) in props {
            let _ = match v {
                AttrVal::Int(n) => self.doc.put(&obj, k.as_str(), *n),
                AttrVal::Bool(b) => self.doc.put(&obj, k.as_str(), *b),
                AttrVal::Str(s) => self.doc.put(&obj, k.as_str(), s.as_str()),
                AttrVal::Float(f) => self.doc.put(&obj, k.as_str(), *f),
            };
        }
    }

    /// Remove a named style from the styles container.
    pub fn remove_style(&mut self, id: &str) {
        let _ = self.doc.delete(&self.c.styles, id);
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

    /// Store `bytes` in the content-addressed blob store and return its blake3
    /// hex hash (the key an `image` block's `blob` attr references). Idempotent —
    /// re-putting identical bytes hashes to the same key. Creates the blobs map
    /// lazily for documents opened under an older schema.
    pub fn put_blob(&mut self, bytes: &[u8]) -> String {
        let hash = blake3::hash(bytes).to_hex().to_string();
        let blobs = match blobs_obj(self.doc, self.c) {
            Some(b) => b,
            None => self.doc.put_object(&self.c.root, KEY_BLOBS, ObjType::Map).unwrap(),
        };
        let _ = self.doc.put(&blobs, hash.as_str(), ScalarValue::Bytes(bytes.to_vec()));
        hash
    }
}

#[cfg(test)]
mod style_tests {
    use super::*;
    use crate::schema::*;
    use automerge::{AutoCommit, ROOT};

    fn props(name: &str, size: f64, bold: bool) -> BTreeMap<String, AttrVal> {
        let mut m = BTreeMap::new();
        m.insert(STYLE_NAME.to_string(), AttrVal::Str(name.into()));
        m.insert(STYLE_SIZE.to_string(), AttrVal::Float(size));
        m.insert(STYLE_BOLD.to_string(), AttrVal::Bool(bold));
        m.insert(STYLE_COLOR.to_string(), AttrVal::Str("#112233".into()));
        m
    }

    #[test]
    fn styles_container_crud_and_snapshot() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.set_style("body", &props("Body", 16.5, false));
            dm.set_style("h1", &props("Heading 1", 30.0, true));
        }

        let mut got = style_ids(&doc, &c);
        got.sort();
        assert_eq!(got, vec!["body".to_string(), "h1".to_string()]);

        let bp = style_props(&doc, &c, "body");
        assert_eq!(bp.get(STYLE_SIZE), Some(&AttrVal::Float(16.5)), "fractional size stored as F64");
        assert_eq!(bp.get(STYLE_NAME), Some(&AttrVal::Str("Body".into())));
        assert_eq!(bp.get(STYLE_BOLD), Some(&AttrVal::Bool(false)));

        // The snapshot exposes the full styles map.
        let snap = materialize(&doc, &c);
        assert_eq!(snap.styles.len(), 2);
        assert_eq!(snap.styles["h1"].get(STYLE_BOLD), Some(&AttrVal::Bool(true)));
    }

    #[test]
    fn doc_props_crud_and_snapshot() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        // No props map yet → empty reads, no panic, open still works.
        assert!(doc_props(&doc, &c).is_empty());
        assert_eq!(doc_prop(&doc, &c, PROP_TITLE), None);
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.set_doc_prop(PROP_TITLE, "Hello");
            dm.set_doc_prop(PROP_AUTHOR, "Ada");
        }
        assert_eq!(doc_prop(&doc, &c, PROP_TITLE).as_deref(), Some("Hello"));
        assert_eq!(doc_prop(&doc, &c, PROP_AUTHOR).as_deref(), Some("Ada"));
        let all = doc_props(&doc, &c);
        assert_eq!(all.get("title").map(String::as_str), Some("Hello"));
        assert_eq!(all.get("author").map(String::as_str), Some("Ada"));
        // The snapshot exposes the properties map.
        let snap = materialize(&doc, &c);
        assert_eq!(snap.properties.get("title").map(String::as_str), Some("Hello"));
        // Field-level upsert: setting one property leaves the other intact.
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.set_doc_prop(PROP_TITLE, "Changed");
        }
        assert_eq!(doc_prop(&doc, &c, PROP_TITLE).as_deref(), Some("Changed"));
        assert_eq!(doc_prop(&doc, &c, PROP_AUTHOR).as_deref(), Some("Ada"), "untouched prop preserved");
    }

    #[test]
    fn open_is_backward_compatible_without_props_map() {
        // A document whose meta map predates the props sub-map must open + read as
        // having no properties (the props map is created lazily on first write).
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        // `init` does not create meta.props; confirm reads degrade gracefully.
        assert!(doc_props(&doc, &c).is_empty(), "no props map → empty");
        assert_eq!(doc_prop(&doc, &c, PROP_GENERATOR), None);
        let bytes = doc.save();
        let reloaded = AutoCommit::load(&bytes).unwrap();
        let c2 = open(&reloaded, &ROOT).unwrap();
        assert!(doc_props(&reloaded, &c2).is_empty(), "reload still opens with no props");
    }

    #[test]
    fn set_style_is_field_level_upsert() {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let mut ids = IdGen::new(doc.get_actor().to_hex_string());
        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.set_style("h1", &props("Heading 1", 30.0, true));
            // Touch only one property; the others must survive (field-level merge).
            let mut one = BTreeMap::new();
            one.insert(STYLE_SIZE.to_string(), AttrVal::Float(40.0));
            dm.set_style("h1", &one);
        }
        let h1 = style_props(&doc, &c, "h1");
        assert_eq!(h1.get(STYLE_SIZE), Some(&AttrVal::Float(40.0)), "updated property");
        assert_eq!(h1.get(STYLE_BOLD), Some(&AttrVal::Bool(true)), "untouched property preserved");
        assert_eq!(h1.get(STYLE_NAME), Some(&AttrVal::Str("Heading 1".into())));

        {
            let mut dm = DocMut { doc: &mut doc, c: &c, ids: &mut ids };
            dm.remove_style("h1");
        }
        assert!(style_ids(&doc, &c).is_empty(), "style removed");
    }
}
