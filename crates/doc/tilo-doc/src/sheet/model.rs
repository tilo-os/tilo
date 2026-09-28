//! The owned [`Workbook`] handle over an automerge `AutoCommit` holding the
//! canonical spreadsheet model, plus the borrowed read/write helpers the codecs
//! drive it through. Mirrors the rich-text [`crate::model`]/[`crate::document`]
//! split, but the spreadsheet shape is simpler: a workbook is an ordered set of
//! sheets, each a sparse map of A1-keyed cells.

use crate::document::DocError;
use crate::sheet::schema::{
    CELL_FORMULA, CELL_STYLE, CELL_TYPE, CELL_VALUE, KEY_SHEET_META, KEY_SHEET_PRESERVED,
    KEY_SHEET_PROPS, KEY_SHEET_SCHEMA, KEY_SHEET_SCHEMA_VERSION, KEY_SHEETS, SHEET_CELLS,
    SHEET_NAME, SHEET_ORDER, SHEET_SCHEMA_ID, SHEET_SCHEMA_VERSION,
};
use crate::sheet::snapshot::{CellType, CellView, SheetView, WorkbookSnapshot};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use std::collections::BTreeMap;

/// The data written into a cell: its value + type, an optional preserved formula
/// (source without the leading `=`), and an optional opaque style ref.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CellInput {
    pub value: String,
    pub cell_type: CellType,
    pub formula: Option<String>,
    pub style: Option<String>,
}

impl CellInput {
    pub fn number(v: impl Into<String>) -> Self {
        Self {
            value: v.into(),
            cell_type: CellType::Number,
            formula: None,
            style: None,
        }
    }
    pub fn text(v: impl Into<String>) -> Self {
        Self {
            value: v.into(),
            cell_type: CellType::Text,
            formula: None,
            style: None,
        }
    }
    /// A formula cell: the formula source (no leading `=`) plus its cached result value and type.
    pub fn formula(src: impl Into<String>, cached: impl Into<String>, ty: CellType) -> Self {
        Self {
            value: cached.into(),
            cell_type: ty,
            formula: Some(src.into()),
            style: None,
        }
    }
    #[must_use]
    pub fn with_style(mut self, style: impl Into<String>) -> Self {
        self.style = Some(style.into());
        self
    }
}

#[derive(Clone, Debug)]
pub struct SheetContainers {
    pub root: ObjId,
    pub sheets: ObjId,
    pub meta: ObjId,
    pub preserved: ObjId,
}

/// Write the schema scalars + create the container maps under `root`.
///
/// # Panics
/// Panics if `root` names an object that doesn't exist in `doc`. Every caller resolves `root`
/// (`automerge::ROOT`, or a namespace node it just created) immediately before calling this,
/// so the object is always live.
#[allow(
    clippy::unwrap_used,
    reason = "root is always a live object at every call site (see the panics note above)"
)]
pub fn init(doc: &mut AutoCommit, root: &ObjId) -> SheetContainers {
    doc.put(root, KEY_SHEET_SCHEMA, SHEET_SCHEMA_ID).unwrap();
    doc.put(root, KEY_SHEET_SCHEMA_VERSION, SHEET_SCHEMA_VERSION)
        .unwrap();
    let sheets = doc.put_object(root, KEY_SHEETS, ObjType::Map).unwrap();
    let meta = doc.put_object(root, KEY_SHEET_META, ObjType::Map).unwrap();
    let preserved = doc
        .put_object(root, KEY_SHEET_PRESERVED, ObjType::Map)
        .unwrap();
    SheetContainers {
        root: root.clone(),
        sheets,
        meta,
        preserved,
    }
}

/// # Errors
/// Returns [`DocError`] if a required container key (`sheets`/`meta`/`preserved`) is missing
/// under `root` or does not resolve to a map.
pub fn open(doc: &AutoCommit, root: &ObjId) -> Result<SheetContainers, DocError> {
    Ok(SheetContainers {
        root: root.clone(),
        sheets: map_at(doc, root, KEY_SHEETS)?,
        meta: map_at(doc, root, KEY_SHEET_META)?,
        preserved: map_at(doc, root, KEY_SHEET_PRESERVED)?,
    })
}

fn map_at(doc: &AutoCommit, root: &ObjId, key: &str) -> Result<ObjId, DocError> {
    match doc.get(root, key)? {
        Some((Value::Object(ObjType::Map), id)) => Ok(id),
        _ => Err(DocError::Invalid(format!(
            "workbook root.{key} is not a map"
        ))),
    }
}

fn sheet_obj(doc: &AutoCommit, c: &SheetContainers, id: &str) -> Option<ObjId> {
    match doc.get(&c.sheets, id) {
        Ok(Some((Value::Object(ObjType::Map), oid))) => Some(oid),
        _ => None,
    }
}

fn sheet_order(doc: &AutoCommit, c: &SheetContainers, id: &str) -> Option<Vec<u8>> {
    let obj = sheet_obj(doc, c, id)?;
    match doc.get(&obj, SHEET_ORDER) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Bytes(b) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Sheet ids in tab order (by fractional order key, then id as a tiebreak so two
/// replicas that merged in different orders converge byte-identically).
pub fn sheet_ids_in_order(doc: &AutoCommit, c: &SheetContainers) -> Vec<String> {
    let mut v: Vec<(Vec<u8>, String)> = doc
        .keys(&c.sheets)
        .filter_map(|id| sheet_order(doc, c, &id).map(|ord| (ord, id)))
        .collect();
    v.sort_by(|a, b| tilo_resource::order::bytes::cmp(&a.0, &b.0).then(a.1.cmp(&b.1)));
    v.into_iter().map(|(_, id)| id).collect()
}

/// A sheet's tab name (empty if unset / unknown id).
pub fn sheet_name(doc: &AutoCommit, c: &SheetContainers, id: &str) -> Option<String> {
    let obj = sheet_obj(doc, c, id)?;
    match doc.get(&obj, SHEET_NAME) {
        Ok(Some((v, _))) => v.as_str().map(std::string::ToString::to_string),
        _ => None,
    }
}

fn cells_obj(doc: &AutoCommit, c: &SheetContainers, id: &str) -> Option<ObjId> {
    let obj = sheet_obj(doc, c, id)?;
    match doc.get(&obj, SHEET_CELLS) {
        Ok(Some((Value::Object(ObjType::Map), cid))) => Some(cid),
        _ => None,
    }
}

fn cell_str(doc: &AutoCommit, cell: &ObjId, key: &str) -> Option<String> {
    match doc.get(cell, key) {
        Ok(Some((v, _))) => v.as_str().map(std::string::ToString::to_string),
        _ => None,
    }
}

/// Materializes a single cell (by A1 ref) into a [`CellView`], or `None` if the ref is unknown or malformed.
pub fn cell(
    doc: &AutoCommit,
    c: &SheetContainers,
    sheet_id: &str,
    reference: &str,
) -> Option<CellView> {
    let cells = cells_obj(doc, c, sheet_id)?;
    let Ok(Some((Value::Object(ObjType::Map), cell_obj))) = doc.get(&cells, reference) else {
        return None;
    };
    materialize_cell(doc, &cell_obj, reference)
}

fn materialize_cell(doc: &AutoCommit, cell_obj: &ObjId, reference: &str) -> Option<CellView> {
    let (col, row) = crate::sheet::a1::parse_ref(reference)?;
    let value = cell_str(doc, cell_obj, CELL_VALUE).unwrap_or_default();
    let cell_type =
        cell_str(doc, cell_obj, CELL_TYPE).map_or(CellType::Text, |s| CellType::from_str(&s));
    let formula = cell_str(doc, cell_obj, CELL_FORMULA).filter(|s| !s.is_empty());
    let style = cell_str(doc, cell_obj, CELL_STYLE).filter(|s| !s.is_empty());
    Some(CellView {
        reference: reference.to_string(),
        col,
        row,
        value,
        cell_type,
        formula,
        style,
    })
}

/// Materialize one sheet into a [`SheetView`] (cells sorted row-major).
pub fn materialize_sheet(doc: &AutoCommit, c: &SheetContainers, id: &str) -> SheetView {
    let name = sheet_name(doc, c, id).unwrap_or_default();
    let mut cells: Vec<CellView> = Vec::new();
    if let Some(cells_obj) = cells_obj(doc, c, id) {
        for reference in doc.keys(&cells_obj) {
            if let Ok(Some((Value::Object(ObjType::Map), cell_obj))) =
                doc.get(&cells_obj, &reference)
                && let Some(view) = materialize_cell(doc, &cell_obj, &reference)
            {
                cells.push(view);
            }
        }
    }
    cells.sort_by_key(|a| (a.row, a.col));
    SheetView {
        id: id.to_string(),
        name,
        cells,
    }
}

fn props_obj(doc: &AutoCommit, c: &SheetContainers) -> Option<ObjId> {
    match doc.get(&c.meta, KEY_SHEET_PROPS) {
        Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
        _ => None,
    }
}

/// All document properties (`name → value`) as a sorted map.
pub fn doc_props(doc: &AutoCommit, c: &SheetContainers) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(props) = props_obj(doc, c) {
        for k in doc.keys(&props) {
            if let Ok(Some((v, _))) = doc.get(&props, &k)
                && let Some(s) = v.as_str()
            {
                out.insert(k, s.to_string());
            }
        }
    }
    out
}

pub fn doc_prop(doc: &AutoCommit, c: &SheetContainers, name: &str) -> Option<String> {
    let props = props_obj(doc, c)?;
    match doc.get(&props, name) {
        Ok(Some((v, _))) => v.as_str().map(std::string::ToString::to_string),
        _ => None,
    }
}

/// All preserved (verbatim, unmodeled) package parts as `(part_name, bytes)`.
pub fn preserved_parts(doc: &AutoCommit, c: &SheetContainers) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for k in doc.keys(&c.preserved) {
        if let Ok(Some((Value::Scalar(s), _))) = doc.get(&c.preserved, &k)
            && let ScalarValue::Bytes(b) = s.as_ref()
        {
            out.push((k, b.clone()));
        }
    }
    out
}

pub fn preserved_part(doc: &AutoCommit, c: &SheetContainers, name: &str) -> Option<Vec<u8>> {
    match doc.get(&c.preserved, name) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Bytes(b) => Some(b.clone()),
            _ => None,
        },
        _ => None,
    }
}

pub fn materialize(doc: &AutoCommit, c: &SheetContainers) -> WorkbookSnapshot {
    let sheets = sheet_ids_in_order(doc, c)
        .into_iter()
        .map(|id| materialize_sheet(doc, c, &id))
        .collect();
    WorkbookSnapshot {
        schema: SHEET_SCHEMA_ID.to_string(),
        schema_version: SHEET_SCHEMA_VERSION,
        sheets,
        properties: doc_props(doc, c),
    }
}

/// Compute a fractional tab-order key placing a sheet after `after` (or at the
/// front when `after` is `None`). Sheets are few, so a full scan is fine.
fn sheet_order_after(doc: &AutoCommit, c: &SheetContainers, after: Option<&str>) -> Vec<u8> {
    let ordered = sheet_ids_in_order(doc, c);
    let pos = match after {
        None => 0,
        Some(id) => ordered
            .iter()
            .position(|x| x == id)
            .map_or(ordered.len(), |i| i.saturating_add(1)),
    };
    let lo = match pos.checked_sub(1).and_then(|i| ordered.get(i)) {
        Some(id) => sheet_order(doc, c, id),
        None => None,
    };
    let hi = match ordered.get(pos) {
        Some(id) => sheet_order(doc, c, id),
        None => None,
    };
    tilo_resource::order::bytes::between(lo.as_deref(), hi.as_deref())
}

/// The owned spreadsheet handle: wraps an automerge document that syncs, versions, and merges like any CRDT.
pub struct Workbook {
    pub(crate) doc: AutoCommit,
    pub(crate) c: SheetContainers,
    ids: crate::model::IdGen,
}

impl Workbook {
    pub fn new() -> Self {
        let mut doc = AutoCommit::new();
        let c = init(&mut doc, &ROOT);
        let ids = crate::model::IdGen::new(doc.get_actor().to_hex_string());
        doc.commit();
        Self { doc, c, ids }
    }

    /// # Errors
    /// Returns [`DocError`] if `bytes` isn't a valid automerge document, or doesn't carry the
    /// workbook schema's container keys.
    pub fn load(bytes: &[u8]) -> Result<Self, DocError> {
        let doc = AutoCommit::load(bytes).map_err(|e| DocError::Load(e.to_string()))?;
        let c = open(&doc, &ROOT)?;
        let ids = seeded_sheet_id_gen(&doc, &c)?;
        Ok(Self { doc, c, ids })
    }

    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    pub fn actor_hex(&self) -> String {
        self.doc.get_actor().to_hex_string()
    }

    /// Add a sheet named `name` at the end of the tab order; returns its id.
    pub fn add_sheet(&mut self, name: &str) -> String {
        let after = self.sheet_ids_in_order().last().cloned();
        self.add_sheet_after(name, after.as_deref())
    }

    /// Add a sheet after the sheet with id `after` (or at the front if `None`).
    ///
    /// # Panics
    /// Panics if `self.c.sheets` no longer resolves in `self.doc` — see the note on
    /// [`Self::set_cell`].
    #[allow(
        clippy::unwrap_used,
        reason = "self.c.sheets was resolved by open()/init() at construction and this API never invalidates it"
    )]
    pub fn add_sheet_after(&mut self, name: &str, after: Option<&str>) -> String {
        let key = sheet_order_after(&self.doc, &self.c, after);
        let id = self.ids.mint();
        let obj = self
            .doc
            .put_object(&self.c.sheets, id.as_str(), ObjType::Map)
            .unwrap();
        self.doc
            .put(&obj, SHEET_ORDER, ScalarValue::Bytes(key))
            .unwrap();
        self.doc.put(&obj, SHEET_NAME, name).unwrap();
        self.doc
            .put_object(&obj, SHEET_CELLS, ObjType::Map)
            .unwrap();
        self.doc.commit();
        id
    }

    /// Rename a sheet (no-op if the id is unknown).
    ///
    /// # Panics
    /// Panics if `self.c.sheets` no longer resolves in `self.doc` — see the note on
    /// [`Self::set_cell`].
    #[allow(
        clippy::unwrap_used,
        reason = "self.c.sheets was resolved by open()/init() at construction and this API never invalidates it"
    )]
    pub fn rename_sheet(&mut self, id: &str, name: &str) {
        if let Some(obj) = sheet_obj(&self.doc, &self.c, id) {
            self.doc.put(&obj, SHEET_NAME, name).unwrap();
            self.doc.commit();
        }
    }

    fn cells_container(&mut self, sheet_id: &str) -> Option<ObjId> {
        let obj = sheet_obj(&self.doc, &self.c, sheet_id)?;
        match self.doc.get(&obj, SHEET_CELLS) {
            Ok(Some((Value::Object(ObjType::Map), cid))) => Some(cid),
            _ => self.doc.put_object(&obj, SHEET_CELLS, ObjType::Map).ok(),
        }
    }

    /// Set (or overwrite) the cell at A1 `reference` on `sheet_id`. Only fields
    /// whose value actually changed are written, so re-setting an identical cell
    /// adds no CRDT churn. `None` for the formula/style clears that field. Returns
    /// `false` if the sheet id is unknown or the reference is malformed.
    ///
    /// # Panics
    /// Panics if `cells` (just resolved-or-created above) stops resolving mid-call, which
    /// cannot happen on this single-threaded, synchronous API.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "pub API used outside qdoc (fs::docstore); signature change out of scope for this crate-only pass"
    )]
    #[allow(
        clippy::unwrap_used,
        reason = "`cells` was just resolved-or-created above, so put_object on it cannot fail mid-call"
    )]
    pub fn set_cell(&mut self, sheet_id: &str, reference: &str, input: CellInput) -> bool {
        if crate::sheet::a1::parse_ref(reference).is_none() {
            return false;
        }
        let Some(cells) = self.cells_container(sheet_id) else {
            return false;
        };
        let cell_obj = match self.doc.get(&cells, reference) {
            Ok(Some((Value::Object(ObjType::Map), o))) => o,
            _ => self
                .doc
                .put_object(&cells, reference, ObjType::Map)
                .unwrap(),
        };
        put_str_if_changed(&mut self.doc, &cell_obj, CELL_VALUE, &input.value);
        put_str_if_changed(
            &mut self.doc,
            &cell_obj,
            CELL_TYPE,
            input.cell_type.as_str(),
        );
        set_opt_str(
            &mut self.doc,
            &cell_obj,
            CELL_FORMULA,
            input.formula.as_deref(),
        );
        set_opt_str(&mut self.doc, &cell_obj, CELL_STYLE, input.style.as_deref());
        self.doc.commit();
        true
    }

    /// Remove a cell entirely (no-op if absent).
    pub fn clear_cell(&mut self, sheet_id: &str, reference: &str) {
        if let Some(cells) = cells_obj(&self.doc, &self.c, sheet_id) {
            let _ = self.doc.delete(&cells, reference);
            self.doc.commit();
        }
    }

    /// Stash a verbatim (unmodeled) package part so export can re-emit it losslessly.
    ///
    /// # Panics
    /// Panics if `self.c.preserved` no longer resolves in `self.doc` — see the note on
    /// [`Self::set_cell`].
    #[allow(
        clippy::unwrap_used,
        reason = "self.c.preserved was resolved by open()/init() at construction and this API never invalidates it"
    )]
    pub fn set_preserved(&mut self, name: &str, bytes: &[u8]) {
        self.doc
            .put(&self.c.preserved, name, ScalarValue::Bytes(bytes.to_vec()))
            .unwrap();
        self.doc.commit();
    }

    /// # Panics
    /// Panics if `self.c.meta` no longer resolves in `self.doc` — see the note on
    /// [`Self::set_cell`].
    #[allow(
        clippy::unwrap_used,
        reason = "self.c.meta was resolved by open()/init() at construction, and `props` was just resolved-or-created above"
    )]
    pub fn set_prop(&mut self, name: &str, value: &str) {
        let props = match self.doc.get(&self.c.meta, KEY_SHEET_PROPS) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => self
                .doc
                .put_object(&self.c.meta, KEY_SHEET_PROPS, ObjType::Map)
                .unwrap(),
        };
        self.doc.put(&props, name, value).unwrap();
        self.doc.commit();
    }

    pub fn sheet_ids_in_order(&self) -> Vec<String> {
        sheet_ids_in_order(&self.doc, &self.c)
    }
    pub fn sheet_name(&self, id: &str) -> Option<String> {
        sheet_name(&self.doc, &self.c, id)
    }
    pub fn cell(&self, sheet_id: &str, reference: &str) -> Option<CellView> {
        cell(&self.doc, &self.c, sheet_id, reference)
    }
    pub fn doc_prop(&self, name: &str) -> Option<String> {
        doc_prop(&self.doc, &self.c, name)
    }
    pub fn preserved_part(&self, name: &str) -> Option<Vec<u8>> {
        preserved_part(&self.doc, &self.c, name)
    }
    pub fn materialize(&self) -> WorkbookSnapshot {
        materialize(&self.doc, &self.c)
    }

    /// Timeline of authored changes (oldest first).
    pub fn versions(&mut self) -> Vec<tilo_resource::history::Version> {
        tilo_resource::history::versions(&mut self.doc)
    }

    /// Reconstruct the workbook as of `version` (time-travel).
    ///
    /// # Errors
    /// Returns [`DocError`] if replaying the changes up to `version` doesn't reproduce a
    /// document carrying the workbook schema's container keys.
    pub fn materialize_at(&mut self, version: usize) -> Result<WorkbookSnapshot, DocError> {
        let rebuilt = tilo_resource::history::replay_prefix(&mut self.doc, version)
            .map_err(|e| DocError::Invalid(format!("apply_changes: {e}")))?;
        let c = open(&rebuilt, &ROOT)?;
        Ok(materialize(&rebuilt, &c))
    }

    /// # Errors
    /// Returns [`DocError`] under the same conditions as [`Self::materialize_at`], for either
    /// `from` or `to`.
    pub fn diff_versions(
        &mut self,
        from: usize,
        to: usize,
    ) -> Result<crate::diff::SheetDiff, DocError> {
        let a = self.materialize_at(from)?;
        let b = self.materialize_at(to)?;
        Ok(crate::diff::diff_workbooks(&a, &b))
    }

    /// Independent replica with a fresh actor id (distinct edit lineage).
    ///
    /// # Panics
    /// Panics if reloading this workbook's own just-saved bytes fails, which cannot happen:
    /// `save()`'s output is always a valid document carrying this workbook's schema.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "reloading this workbook's own just-saved, schema-valid bytes cannot fail"
    )]
    pub fn fork(&mut self) -> Workbook {
        let bytes = self.save();
        let mut doc = AutoCommit::load(&bytes).expect("fork load");
        doc.set_actor(automerge::ActorId::random());
        let c = open(&doc, &ROOT).expect("fork open");
        let ids = crate::model::IdGen::new(doc.get_actor().to_hex_string());
        Workbook { doc, c, ids }
    }

    /// Merge another replica's changes into this one (low-level merge to sidestep
    /// the automerge-0.8 `AutoCommit::merge` patch-log panic, as the rich-text
    /// [`crate::Document::merge`] does).
    ///
    /// # Panics
    /// Panics if `self`'s or `other`'s own just-saved bytes fail to reload, or the reloaded
    /// document loses the workbook schema — neither can happen: both are this type's own
    /// `save()` output, and merging never removes the schema's container keys.
    #[allow(
        clippy::expect_used,
        reason = "reloading/re-merging this workbook's own schema-valid bytes cannot fail"
    )]
    pub fn merge(&mut self, other: &mut Workbook) {
        let merged = {
            let mut a = automerge::Automerge::load(&self.doc.save()).expect("merge: load self");
            let mut b = automerge::Automerge::load(&other.doc.save()).expect("merge: load other");
            let _ = a.merge(&mut b);
            a.save()
        };
        let candidate_doc = AutoCommit::load(&merged).expect("merge: reload");
        let candidate_containers = open(&candidate_doc, &ROOT).expect("merge: open");
        let candidate_ids = seeded_sheet_id_gen(&candidate_doc, &candidate_containers)
            .expect("merged workbook exhausted its id namespace; refusing to reuse an identifier");
        self.doc = candidate_doc;
        self.c = candidate_containers;
        self.ids = candidate_ids;
    }
}

impl Default for Workbook {
    fn default() -> Self {
        Self::new()
    }
}

/// An [`crate::model::IdGen`] for `doc`'s actor, seeded past existing sheet ids so
/// a generator over a reloaded workbook won't re-mint (and overwrite) a sheet.
fn seeded_sheet_id_gen(
    doc: &AutoCommit,
    c: &SheetContainers,
) -> Result<crate::model::IdGen, crate::model::IdGenError> {
    let actor = doc.get_actor().to_hex_string();
    let prefix = format!("{actor}.");
    let mut next = Some(0u64);
    for id in doc.keys(&c.sheets) {
        if let Some(rest) = id.strip_prefix(&prefix)
            && let Ok(n) = rest.parse::<u64>()
        {
            next = match (next, n.checked_add(1)) {
                (Some(current), Some(candidate)) => Some(current.max(candidate)),
                (_, None) | (None, _) => None,
            };
        }
    }
    let next = next.ok_or_else(|| crate::model::IdGenError::Exhausted {
        actor_hex: actor.clone(),
    })?;
    Ok(crate::model::IdGen::with_counter(actor, next))
}

/// Write a string field only when it differs from the stored value (avoids a
/// redundant CRDT op — the store-churn guard).
///
/// # Panics
/// Panics if `obj` no longer resolves in `doc`. Both call sites pass a cell object just
/// resolved-or-created a line above, so it is always live.
#[allow(
    clippy::unwrap_used,
    reason = "obj is always a live object at every call site (see the panics note above)"
)]
fn put_str_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: &str) {
    let current = match doc.get(obj, key) {
        Ok(Some((v, _))) => v.as_str().map(std::string::ToString::to_string),
        _ => None,
    };
    if current.as_deref() != Some(value) {
        doc.put(obj, key, value).unwrap();
    }
}

/// Set an optional string field: write it when `Some` and changed, delete it when
/// `None` and currently present.
///
/// # Panics
/// Panics if `obj` no longer resolves in `doc` — see the note on [`put_str_if_changed`].
#[allow(
    clippy::unwrap_used,
    reason = "obj is always a live object at every call site (see the panics note above)"
)]
fn set_opt_str(doc: &mut AutoCommit, obj: &ObjId, key: &str, value: Option<&str>) {
    let current = match doc.get(obj, key) {
        Ok(Some((v, _))) => v.as_str().map(std::string::ToString::to_string),
        _ => None,
    };
    if current.as_deref() == value {
        return; // already matches: no CRDT op needed
    }
    match value {
        Some(v) => doc.put(obj, key, v).unwrap(),
        None => {
            let _ = doc.delete(obj, key);
        }
    }
}
