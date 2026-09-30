//! Automerge document model for the workbook.
//!
//! State lives in the CRDT (`ui::State::doc`), so two peers editing the
//! same workbook converge for free. Layout:
//!
//! ```text
//! root
//! └── "wb" (Map)
//!     └── "sheets" (List)
//!          └── [i] (Map)
//!              ├── "name"  (Str)
//!              └── "cells" (Map)      # key "col,row" -> Map
//!                   └── "<c>,<r>" (Map)
//!                        ├── "raw" (Str)   # raw input: "=SUM(A1:A3)" or "42"
//!                        ├── "fmt" (Str)   # CellFormat::as_str()
//!                        └── "b"   (Bool)  # bold text; absent means regular
//! ```
//!
//! Cells are keyed by a `"col,row"` string (zero-based) rather than A1 so
//! the key is cheap to build/parse and stable across column renames.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use ui::doc::{read_bool, read_i64_coerced, read_str};

use crate::model::{Addr, CellFormat, Currency, DateStyle, GridEdit, TimeStyle};
use crate::settings::DocSettings;

/// One sheet's snapshot pulled out of the CRDT for rendering.
#[derive(Clone, Debug, Default)]
pub struct SheetSnapshot {
    pub name: String,
    /// raw input string per populated cell
    pub raw: std::collections::HashMap<Addr, String>,
    /// per-cell format (only present when not General)
    pub fmt: std::collections::HashMap<Addr, CellFormat>,
    /// cells whose text is bold
    pub bold: std::collections::HashSet<Addr>,
}

/// The first chart set deliberately stays compact: it is sufficient for the
/// common trend/comparison workflows while keeping every chart a bounded CRDT
/// record rather than an opaque rendering blob.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartKind {
    Column,
    Line,
}

impl ChartKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::Column => "column",
            Self::Line => "line",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "column" => Self::Column,
            "line" => Self::Line,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartSnapshot {
    pub id: String,
    pub title: String,
    pub kind: ChartKind,
    pub start: Addr,
    pub end: Addr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentReply {
    pub id: String,
    pub author: String,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommentThread {
    pub id: String,
    pub start: Addr,
    pub end: Addr,
    pub author: String,
    pub body: String,
    pub resolved: bool,
    pub replies: Vec<CommentReply>,
}

/// Stateless namespace of workbook document helpers.
pub struct SheetsDoc;

impl SheetsDoc {
    /// Annotation roots are created with every sheet.  In particular they are
    /// *not* lazy: two peers adding the first chart/comment from the same
    /// empty sheet must write into the same already-replicated map, rather
    /// than racing to install incompatible child objects at one map key.
    fn seed_annotation_roots(doc: &mut AutoCommit, sheet: &ObjId) {
        let _ = doc.put_object(sheet, "charts", ObjType::Map);
        let _ = doc.put_object(sheet, "comments", ObjType::Map);
        // Replies are records keyed by their own UUID and point at a parent
        // thread. Keeping them beside comments avoids a concurrent first
        // reply racing to create a nested `replies` object.
        let _ = doc.put_object(sheet, "comment_replies", ObjType::Map);
    }

    /// Return every visible object for a key, including Automerge conflict
    /// branches.  Old workbooks could have lazily-created annotation maps;
    /// retaining all branches when materialising makes their already-merged
    /// data durable and visible while fresh sheets use the eager single root.
    fn map_values(doc: &AutoCommit, parent: &ObjId, key: &str) -> Vec<ObjId> {
        doc.get_all(parent, key)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(value, id)| matches!(value, Value::Object(ObjType::Map)).then_some(id))
            .collect()
    }

    fn annotation_map(doc: &mut AutoCommit, idx: usize, key: &str) -> Option<ObjId> {
        let sheet = Self::sheet_map(doc, idx)?;
        if let Some(map) = Self::map_values(doc, &sheet, key).into_iter().next() {
            Some(map)
        } else {
            // Legacy workbook migration. A simultaneous migration can leave
            // multiple maps, which `map_values` above deliberately reads.
            Some(Self::put_object_or_panic(doc, &sheet, key, ObjType::Map))
        }
    }

    fn annotation_maps(doc: &mut AutoCommit, idx: usize, key: &str) -> Vec<ObjId> {
        let Some(sheet) = Self::sheet_map(doc, idx) else {
            return Vec::new();
        };
        Self::map_values(doc, &sheet, key)
    }
    fn cell_key(addr: Addr) -> String {
        format!("{},{}", addr.0, addr.1)
    }

    fn parse_cell_key(key: &str) -> Option<Addr> {
        let (c, r) = key.split_once(',')?;
        Some((c.parse().ok()?, r.parse().ok()?))
    }

    /// `doc.put_object` cannot fail for a key written into a Map this module
    /// just created or looked up moments before, within the same synchronous
    /// mutation — no other actor can intervene while `&mut AutoCommit` is
    /// held. The underlying API still returns `Result` to cover the general
    /// case (e.g. a stale `ObjId` a peer deleted since the document loaded).
    #[allow(
        clippy::expect_used,
        reason = "obj was just created or verified to exist in this same synchronous mutation"
    )]
    fn put_object_or_panic(doc: &mut AutoCommit, obj: &ObjId, prop: &str, ty: ObjType) -> ObjId {
        doc.put_object(obj, prop, ty)
            .expect("put_object on a freshly-verified object never fails")
    }

    /// `doc.insert_object` cannot fail for an index within `0..=len` of a
    /// List this module just looked up within the same synchronous mutation.
    /// See [`Self::put_object_or_panic`].
    #[allow(
        clippy::expect_used,
        reason = "index and list were just established in this same synchronous mutation"
    )]
    fn insert_object_or_panic(doc: &mut AutoCommit, obj: &ObjId, idx: usize, ty: ObjType) -> ObjId {
        doc.insert_object(obj, idx, ty)
            .expect("insert_object at a freshly-verified index never fails")
    }

    /// Get-or-create the top-level `wb` Map.
    pub fn wb_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "wb") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => Self::put_object_or_panic(doc, &ROOT, "wb", ObjType::Map),
        }
    }

    /// Get-or-create the `sheets` List under `wb`.
    pub fn sheets_list(doc: &mut AutoCommit) -> ObjId {
        let wb = Self::wb_map(doc);
        match doc.get(&wb, "sheets") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => Self::put_object_or_panic(doc, &wb, "sheets", ObjType::List),
        }
    }

    /// Number of sheets currently in the document.
    pub fn sheet_count(doc: &mut AutoCommit) -> usize {
        let sheets = Self::sheets_list(doc);
        doc.length(&sheets)
    }

    /// Read the current sheet count without creating a malformed/missing
    /// workbook subtree. Action preflight uses this so refusing a stale or
    /// hostile envelope has no side effects.
    pub fn sheet_count_readonly(doc: &AutoCommit) -> usize {
        let Ok(Some((Value::Object(ObjType::Map), wb))) = doc.get(&ROOT, "wb") else {
            return 0;
        };
        let Ok(Some((Value::Object(ObjType::List), sheets))) = doc.get(&wb, "sheets") else {
            return 0;
        };
        doc.length(&sheets)
    }

    /// Whether `idx` still identifies a concrete sheet map. This is the
    /// read-only counterpart to `sheet_map`, for atomic action validation.
    pub fn sheet_exists(doc: &AutoCommit, idx: usize) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), wb))) = doc.get(&ROOT, "wb") else {
            return false;
        };
        let Ok(Some((Value::Object(ObjType::List), sheets))) = doc.get(&wb, "sheets") else {
            return false;
        };
        matches!(
            doc.get(&sheets, idx),
            Ok(Some((Value::Object(ObjType::Map), _)))
        )
    }

    /// Whether the workbook has been seeded at all.
    pub fn is_empty(doc: &mut AutoCommit) -> bool {
        Self::sheet_count(doc) == 0
    }

    /// The Map `ObjId` for sheet `idx`, if it exists.
    fn sheet_map(doc: &mut AutoCommit, idx: usize) -> Option<ObjId> {
        let sheets = Self::sheets_list(doc);
        match doc.get(&sheets, idx) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
    }

    /// Stable logical identity for history, search and the action palette.
    /// Structural moves copy it into the replacement map so identity survives
    /// index and name changes.
    pub fn sheet_resource_id(doc: &AutoCommit, idx: usize) -> Option<String> {
        let Ok(Some((Value::Object(ObjType::Map), wb))) = doc.get(&ROOT, "wb") else {
            return None;
        };
        let Ok(Some((Value::Object(ObjType::List), sheets))) = doc.get(&wb, "sheets") else {
            return None;
        };
        let Ok(Some((Value::Object(ObjType::Map), sheet))) = doc.get(&sheets, idx) else {
            return None;
        };
        read_str(doc, &sheet, "resource_id")
    }

    /// Resolve a durable sheet identity to its current list position. This is
    /// deliberately linear: sheet counts are small and correctness across
    /// concurrent reorder matters more than caching a positional alias.
    pub fn sheet_index_by_resource_id(doc: &AutoCommit, wanted: &str) -> Option<usize> {
        let (Value::Object(ObjType::Map), wb) = doc.get(&ROOT, "wb").ok()?? else {
            return None;
        };
        let (Value::Object(ObjType::List), sheets) = doc.get(&wb, "sheets").ok()?? else {
            return None;
        };
        (0..doc.length(&sheets))
            .find(|&idx| Self::sheet_resource_id(doc, idx).as_deref() == Some(wanted))
    }

    /// The cells an applied change touched on sheet `idx`, read from its
    /// patches: a cell's key names it whether the cell or a field in it changed.
    pub fn changed_cells(doc: &AutoCommit, idx: usize, changes: &[automerge::Patch]) -> Vec<Addr> {
        let cells = (|| {
            let (Value::Object(ObjType::Map), wb) = doc.get(&ROOT, "wb").ok()?? else {
                return None;
            };
            let (Value::Object(ObjType::List), sheets) = doc.get(&wb, "sheets").ok()?? else {
                return None;
            };
            let (Value::Object(ObjType::Map), sheet) = doc.get(&sheets, idx).ok()?? else {
                return None;
            };
            let (Value::Object(ObjType::Map), cells) = doc.get(&sheet, "cells").ok()?? else {
                return None;
            };
            Some(cells)
        })();
        let Some(cells) = cells else {
            return Vec::new();
        };
        let mut touched: Vec<Addr> = changes
            .iter()
            .filter_map(|patch| {
                let key = if patch.obj == cells {
                    match &patch.action {
                        automerge::PatchAction::PutMap { key, .. }
                        | automerge::PatchAction::DeleteMap { key } => key.as_str(),
                        _ => return None,
                    }
                } else {
                    match patch.path.iter().find(|(obj, _)| *obj == cells) {
                        Some((_, automerge::Prop::Map(key))) => key.as_str(),
                        _ => return None,
                    }
                };
                Self::parse_cell_key(key)
            })
            .collect();
        touched.sort_unstable();
        touched.dedup();
        touched
    }

    /// The file sheet `idx` was imported from, if any.
    pub fn sheet_source(doc: &AutoCommit, idx: usize) -> Option<String> {
        let (Value::Object(ObjType::Map), wb) = doc.get(&ROOT, "wb").ok()?? else {
            return None;
        };
        let (Value::Object(ObjType::List), sheets) = doc.get(&wb, "sheets").ok()?? else {
            return None;
        };
        let (Value::Object(ObjType::Map), sheet) = doc.get(&sheets, idx).ok()?? else {
            return None;
        };
        read_str(doc, &sheet, "source")
    }

    /// Record the file sheet `idx` was imported from.
    pub fn set_sheet_source(doc: &mut AutoCommit, idx: usize, source: &str) {
        if let Some(sheet) = Self::sheet_map(doc, idx) {
            let _ = doc.put(&sheet, "source", source);
        }
    }

    /// The first sheet imported from `source`.
    pub fn sheet_index_by_source(doc: &AutoCommit, source: &str) -> Option<usize> {
        (0..Self::sheet_count_readonly(doc))
            .find(|&idx| Self::sheet_source(doc, idx).as_deref() == Some(source))
    }

    /// The `cells` Map for sheet `idx`, creating it if missing.
    fn cells_map(doc: &mut AutoCommit, idx: usize) -> Option<ObjId> {
        let sheet = Self::sheet_map(doc, idx)?;
        Some(match doc.get(&sheet, "cells") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => Self::put_object_or_panic(doc, &sheet, "cells", ObjType::Map),
        })
    }

    /// Append a new sheet with `name` and return its index.
    pub fn add_sheet(doc: &mut AutoCommit, name: &str) -> usize {
        Self::add_sheet_with_id(doc, name, "")
    }

    /// Append a sheet carrying the action-authored durable identity. Empty is
    /// stores the new sheet's own object id, for local seeding.
    pub fn add_sheet_with_id(doc: &mut AutoCommit, name: &str, resource_id: &str) -> usize {
        let sheets = Self::sheets_list(doc);
        let idx = doc.length(&sheets);
        let sheet = Self::insert_object_or_panic(doc, &sheets, idx, ObjType::Map);
        let _ = doc.put(
            &sheet,
            "resource_id",
            if resource_id.is_empty() {
                sheet.to_string()
            } else {
                resource_id.to_string()
            },
        );
        let _ = doc.put(&sheet, "name", name);
        let _ = doc.put_object(&sheet, "cells", ObjType::Map);
        Self::seed_annotation_roots(doc, &sheet);
        idx
    }

    /// Rename sheet `idx`.
    pub fn rename_sheet(doc: &mut AutoCommit, idx: usize, name: &str) {
        if let Some(sheet) = Self::sheet_map(doc, idx) {
            let _ = doc.put(&sheet, "name", name);
        }
    }

    /// Insert a new blank sheet named `name` at position `at` (clamped to the
    /// end) and return the index it landed at.
    pub fn insert_sheet(doc: &mut AutoCommit, at: usize, name: &str) -> usize {
        Self::insert_sheet_with_id(doc, at, name, "")
    }

    /// Insert a sheet using the exact durable id serialized in an action.
    pub fn insert_sheet_with_id(
        doc: &mut AutoCommit,
        at: usize,
        name: &str,
        resource_id: &str,
    ) -> usize {
        let sheets = Self::sheets_list(doc);
        let at = at.min(doc.length(&sheets));
        let sheet = Self::insert_object_or_panic(doc, &sheets, at, ObjType::Map);
        let _ = doc.put(
            &sheet,
            "resource_id",
            if resource_id.is_empty() {
                sheet.to_string()
            } else {
                resource_id.to_string()
            },
        );
        let _ = doc.put(&sheet, "name", name);
        let _ = doc.put_object(&sheet, "cells", ObjType::Map);
        Self::seed_annotation_roots(doc, &sheet);
        at
    }

    /// Delete sheet `idx`. Refuses to remove the final sheet so a workbook
    /// always keeps at least one sheet. Returns whether a sheet was removed.
    pub fn delete_sheet(doc: &mut AutoCommit, idx: usize) -> bool {
        let sheets = Self::sheets_list(doc);
        if doc.length(&sheets) <= 1 || idx >= doc.length(&sheets) {
            return false;
        }
        doc.delete(&sheets, idx).is_ok()
    }

    /// Duplicate sheet `idx` (name, per-cell raw+format, and tab colour) into a
    /// new sheet inserted immediately after it. Returns the new sheet's index.
    pub fn duplicate_sheet(doc: &mut AutoCommit, idx: usize) -> Option<usize> {
        Self::duplicate_sheet_with_id(doc, idx, "")
    }

    /// Duplicate with an action-authored identity so independent replays keep
    /// the new logical sheet address stable.
    pub fn duplicate_sheet_with_id(
        doc: &mut AutoCommit,
        idx: usize,
        resource_id: &str,
    ) -> Option<usize> {
        Self::sheet_map(doc, idx)?;
        let snap = Self::snapshot_sheet(doc, idx, Currency::USD);
        let color = Self::sheet_color(doc, idx);
        let new_name = format!("{} copy", snap.name);
        let new_idx = Self::insert_sheet_with_id(doc, idx + 1, &new_name, resource_id);
        for (addr, raw) in &snap.raw {
            Self::set_cell_raw(doc, new_idx, *addr, raw);
        }
        for (addr, fmt) in &snap.fmt {
            Self::set_cell_format(doc, new_idx, *addr, *fmt);
        }
        for addr in &snap.bold {
            Self::set_cell_bold(doc, new_idx, *addr, true);
        }
        Self::copy_annotations(doc, idx, new_idx);
        if let Some(c) = color {
            Self::set_sheet_color(doc, new_idx, Some(c));
        }
        Some(new_idx)
    }

    /// Move sheet `from` so it ends up at index `to`, preserving its name,
    /// cells, formats and colour. Implemented as remove-then-reinsert.
    pub fn move_sheet(doc: &mut AutoCommit, from: usize, to: usize) {
        let len = Self::sheet_count(doc);
        if from >= len || to >= len || from == to {
            return;
        }
        let snap = Self::snapshot_sheet(doc, from, Currency::USD);
        let color = Self::sheet_color(doc, from);
        let resource_id = Self::sheet_resource_id(doc, from);
        let source = Self::sheet_source(doc, from);
        let charts = Self::charts(doc, from);
        let comments = Self::comments(doc, from);
        let sheets = Self::sheets_list(doc);
        let _ = doc.delete(&sheets, from);
        let insert_at = to.min(len - 1);
        let sheet = Self::insert_object_or_panic(doc, &sheets, insert_at, ObjType::Map);
        if let Some(resource_id) = resource_id {
            let _ = doc.put(&sheet, "resource_id", resource_id);
        }
        if let Some(source) = source {
            let _ = doc.put(&sheet, "source", source);
        }
        let _ = doc.put(&sheet, "name", &snap.name);
        let _ = doc.put_object(&sheet, "cells", ObjType::Map);
        for (addr, raw) in &snap.raw {
            Self::set_cell_raw(doc, insert_at, *addr, raw);
        }
        for (addr, fmt) in &snap.fmt {
            Self::set_cell_format(doc, insert_at, *addr, *fmt);
        }
        for addr in &snap.bold {
            Self::set_cell_bold(doc, insert_at, *addr, true);
        }
        for chart in charts {
            Self::put_chart(doc, insert_at, &chart);
        }
        for thread in comments {
            Self::put_comment(doc, insert_at, &thread);
        }
        if let Some(c) = color {
            Self::set_sheet_color(doc, insert_at, Some(c));
        }
    }

    /// A sheet's tab colour, stored as a palette index. `None` means no colour.
    pub fn sheet_color(doc: &mut AutoCommit, idx: usize) -> Option<u32> {
        let sheet = Self::sheet_map(doc, idx)?;
        read_i64_coerced(doc, &sheet, "color").map(|n| n as u32)
    }

    /// Set (or clear, with `None`) a sheet's tab colour palette index.
    pub fn set_sheet_color(doc: &mut AutoCommit, idx: usize, color: Option<u32>) {
        if let Some(sheet) = Self::sheet_map(doc, idx) {
            match color {
                Some(c) => {
                    let _ = doc.put(&sheet, "color", i64::from(c));
                }
                None => {
                    let _ = doc.delete(&sheet, "color");
                }
            }
        }
    }

    /// Every sheet's tab colour in order (parallel to [`Self::sheet_names`]).
    pub fn sheet_colors(doc: &mut AutoCommit) -> Vec<Option<u32>> {
        let n = Self::sheet_count(doc);
        (0..n).map(|i| Self::sheet_color(doc, i)).collect()
    }

    /// All sheet names in order.
    pub fn sheet_names(doc: &mut AutoCommit) -> Vec<String> {
        let sheets = Self::sheets_list(doc);
        let n = doc.length(&sheets);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let name = match doc.get(&sheets, i) {
                Ok(Some((Value::Object(ObjType::Map), sheet))) => {
                    read_str(doc, &sheet, "name").unwrap_or_default()
                }
                _ => String::new(),
            };
            out.push(if name.is_empty() {
                format!("Sheet{}", i + 1)
            } else {
                name
            });
        }
        out
    }

    /// Set (or clear) a cell's raw input. An empty string removes the
    /// cell entry entirely so the sheet stays sparse.
    pub fn set_cell_raw(doc: &mut AutoCommit, idx: usize, addr: Addr, raw: &str) {
        let Some(cells) = Self::cells_map(doc, idx) else {
            return;
        };
        let key = Self::cell_key(addr);
        if raw.is_empty() {
            // Keep any format but clear the value; if both gone, delete.
            if let Ok(Some((Value::Object(ObjType::Map), cell))) = doc.get(&cells, key.as_str()) {
                let has_fmt = matches!(doc.get(&cell, "fmt"), Ok(Some(_)))
                    || matches!(doc.get(&cell, "b"), Ok(Some(_)));
                if has_fmt {
                    let _ = doc.put(&cell, "raw", "");
                } else {
                    let _ = doc.delete(&cells, key.as_str());
                }
            }
            return;
        }
        let cell = match doc.get(&cells, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => Self::put_object_or_panic(doc, &cells, key.as_str(), ObjType::Map),
        };
        let _ = doc.put(&cell, "raw", raw);
    }

    /// Set a cell's display format.
    pub fn set_cell_format(doc: &mut AutoCommit, idx: usize, addr: Addr, fmt: CellFormat) {
        let Some(cells) = Self::cells_map(doc, idx) else {
            return;
        };
        let key = Self::cell_key(addr);
        let cell = match doc.get(&cells, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => Self::put_object_or_panic(doc, &cells, key.as_str(), ObjType::Map),
        };
        let _ = doc.put(&cell, "fmt", fmt.as_str());
    }

    /// Make a cell's text bold, or regular again. A regular cell stores no
    /// flag, and one left with neither value nor format disappears.
    pub fn set_cell_bold(doc: &mut AutoCommit, idx: usize, addr: Addr, bold: bool) {
        let Some(cells) = Self::cells_map(doc, idx) else {
            return;
        };
        let key = Self::cell_key(addr);
        if bold {
            let cell = match doc.get(&cells, key.as_str()) {
                Ok(Some((Value::Object(ObjType::Map), id))) => id,
                _ => Self::put_object_or_panic(doc, &cells, key.as_str(), ObjType::Map),
            };
            let _ = doc.put(&cell, "b", true);
            return;
        }
        let Ok(Some((Value::Object(ObjType::Map), cell))) = doc.get(&cells, key.as_str()) else {
            return;
        };
        if matches!(doc.get(&cell, "b"), Ok(Some(_))) {
            let _ = doc.delete(&cell, "b");
        }
        let empty_raw = read_str(doc, &cell, "raw").is_none_or(|raw| raw.is_empty());
        if empty_raw && !matches!(doc.get(&cell, "fmt"), Ok(Some(_))) {
            let _ = doc.delete(&cells, key.as_str());
        }
    }

    /// Get-or-create the per-document `settings` Map under `wb`.
    fn settings_map(doc: &mut AutoCommit) -> ObjId {
        let wb = Self::wb_map(doc);
        match doc.get(&wb, "settings") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => Self::put_object_or_panic(doc, &wb, "settings", ObjType::Map),
        }
    }

    /// Read the per-document locale overrides from the CRDT. Absent fields
    /// mean "inherit the global default".
    pub fn doc_settings(doc: &mut AutoCommit) -> DocSettings {
        let s = Self::settings_map(doc);
        DocSettings {
            currency: read_str(doc, &s, "currency").and_then(|c| Currency::from_code(&c)),
            date_format: read_str(doc, &s, "date_format").and_then(|v| parse_date_style(&v)),
            time_format: read_str(doc, &s, "time_format").and_then(|v| parse_time_style(&v)),
        }
    }

    /// Persist the per-document locale overrides. A `None` field clears the
    /// stored override so the document falls back to the global default.
    pub fn set_doc_settings(doc: &mut AutoCommit, settings: &DocSettings) {
        let s = Self::settings_map(doc);
        match settings.currency {
            Some(c) => {
                let _ = doc.put(&s, "currency", c.code());
            }
            None => {
                let _ = doc.delete(&s, "currency");
            }
        }
        match settings.date_format {
            Some(d) => {
                let _ = doc.put(&s, "date_format", date_style_code(d));
            }
            None => {
                let _ = doc.delete(&s, "date_format");
            }
        }
        match settings.time_format {
            Some(t) => {
                let _ = doc.put(&s, "time_format", time_style_code(t));
            }
            None => {
                let _ = doc.delete(&s, "time_format");
            }
        }
    }

    /// Read the raw input of a single cell.
    pub fn cell_raw(doc: &mut AutoCommit, idx: usize, addr: Addr) -> String {
        let Some(cells) = Self::cells_map(doc, idx) else {
            return String::new();
        };
        let key = Self::cell_key(addr);
        match doc.get(&cells, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), cell))) => {
                read_str(doc, &cell, "raw").unwrap_or_default()
            }
            _ => String::new(),
        }
    }

    /// Upsert one stable chart record. Actions preflight all fields before this
    /// method runs, so identical replay emits the same scalar writes.
    pub fn put_chart(doc: &mut AutoCommit, idx: usize, chart: &ChartSnapshot) {
        let Some(charts) = Self::annotation_map(doc, idx, "charts") else {
            return;
        };
        let chart_obj = match doc.get(&charts, chart.id.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => Self::put_object_or_panic(doc, &charts, chart.id.as_str(), ObjType::Map),
        };
        let _ = doc.put(&chart_obj, "title", &chart.title);
        let _ = doc.put(&chart_obj, "kind", chart.kind.code());
        let _ = doc.put(&chart_obj, "c0", i64::from(chart.start.0));
        let _ = doc.put(&chart_obj, "r0", i64::from(chart.start.1));
        let _ = doc.put(&chart_obj, "c1", i64::from(chart.end.0));
        let _ = doc.put(&chart_obj, "r1", i64::from(chart.end.1));
    }

    pub fn delete_chart(doc: &mut AutoCommit, idx: usize, id: &str) {
        for charts in Self::annotation_maps(doc, idx, "charts") {
            let _ = doc.delete(&charts, id);
        }
    }

    pub fn charts(doc: &mut AutoCommit, idx: usize) -> Vec<ChartSnapshot> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for charts in Self::annotation_maps(doc, idx, "charts") {
            for id in doc.keys(&charts) {
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Ok(Some((Value::Object(ObjType::Map), chart))) = doc.get(&charts, id.as_str())
                else {
                    continue;
                };
                let value = |key: &str| read_str(doc, &chart, key);
                let number = |key: &str| {
                    doc.get(&chart, key)
                        .ok()
                        .flatten()
                        .and_then(|(v, _)| match v {
                            Value::Scalar(v) => v.as_i64(),
                            Value::Object(_) => None,
                        })
                        .and_then(|v| u32::try_from(v).ok())
                };
                let (Some(title), Some(kind), Some(c0), Some(r0), Some(c1), Some(r1)) = (
                    value("title"),
                    value("kind").and_then(|v| ChartKind::parse(&v)),
                    number("c0"),
                    number("r0"),
                    number("c1"),
                    number("r1"),
                ) else {
                    continue;
                };
                out.push(ChartSnapshot {
                    id,
                    title,
                    kind,
                    start: (c0, r0),
                    end: (c1, r1),
                });
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn put_comment(doc: &mut AutoCommit, idx: usize, thread: &CommentThread) {
        let Some(comments) = Self::annotation_map(doc, idx, "comments") else {
            return;
        };
        let object = match doc.get(&comments, thread.id.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => Self::put_object_or_panic(doc, &comments, thread.id.as_str(), ObjType::Map),
        };
        let _ = doc.put(&object, "author", &thread.author);
        let _ = doc.put(&object, "body", &thread.body);
        let _ = doc.put(&object, "resolved", thread.resolved);
        let _ = doc.put(&object, "c0", i64::from(thread.start.0));
        let _ = doc.put(&object, "r0", i64::from(thread.start.1));
        let _ = doc.put(&object, "c1", i64::from(thread.end.0));
        let _ = doc.put(&object, "r1", i64::from(thread.end.1));
        for reply in &thread.replies {
            Self::put_comment_reply(doc, idx, &thread.id, reply);
        }
    }

    /// Append/update a reply in the eager sheet-level reply map. The stable
    /// reply UUID means concurrent first replies never compete for a nested
    /// child-map key and therefore both survive a merge.
    pub fn put_comment_reply(
        doc: &mut AutoCommit,
        idx: usize,
        parent_id: &str,
        reply: &CommentReply,
    ) {
        let Some(replies) = Self::annotation_map(doc, idx, "comment_replies") else {
            return;
        };
        let object = match doc.get(&replies, reply.id.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => Self::put_object_or_panic(doc, &replies, reply.id.as_str(), ObjType::Map),
        };
        let _ = doc.put(&object, "parent_id", parent_id);
        let _ = doc.put(&object, "author", &reply.author);
        let _ = doc.put(&object, "body", &reply.body);
    }

    pub fn set_comment_resolved(doc: &mut AutoCommit, idx: usize, id: &str, resolved: bool) {
        for comments in Self::annotation_maps(doc, idx, "comments") {
            if let Ok(Some((Value::Object(ObjType::Map), object))) = doc.get(&comments, id) {
                let _ = doc.put(&object, "resolved", resolved);
            }
        }
    }

    pub fn edit_comment_body(doc: &mut AutoCommit, idx: usize, id: &str, body: &str) {
        for comments in Self::annotation_maps(doc, idx, "comments") {
            if let Ok(Some((Value::Object(ObjType::Map), object))) = doc.get(&comments, id) {
                let _ = doc.put(&object, "body", body);
            }
        }
    }

    pub fn delete_comment(doc: &mut AutoCommit, idx: usize, id: &str) {
        for comments in Self::annotation_maps(doc, idx, "comments") {
            let _ = doc.delete(&comments, id);
        }
        // Replies are independent records in the current schema; remove the
        // whole thread in the same deterministic action.
        for replies in Self::annotation_maps(doc, idx, "comment_replies") {
            let ids: Vec<String> = doc.keys(&replies).collect();
            for reply_id in ids {
                if let Ok(Some((Value::Object(ObjType::Map), reply))) =
                    doc.get(&replies, reply_id.as_str())
                    && read_str(doc, &reply, "parent_id").as_deref() == Some(id)
                {
                    let _ = doc.delete(&replies, reply_id);
                }
            }
        }
    }

    pub fn comments(doc: &mut AutoCommit, idx: usize) -> Vec<CommentThread> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for comments in Self::annotation_maps(doc, idx, "comments") {
            for id in doc.keys(&comments) {
                if !seen.insert(id.clone()) {
                    continue;
                }
                let Ok(Some((Value::Object(ObjType::Map), object))) =
                    doc.get(&comments, id.as_str())
                else {
                    continue;
                };
                let value = |key: &str| read_str(doc, &object, key);
                let number = |key: &str| {
                    doc.get(&object, key)
                        .ok()
                        .flatten()
                        .and_then(|(v, _)| match v {
                            Value::Scalar(v) => v.as_i64(),
                            Value::Object(_) => None,
                        })
                        .and_then(|v| u32::try_from(v).ok())
                };
                let resolved = read_bool(doc, &object, "resolved").unwrap_or(false);
                let (Some(author), Some(body), Some(c0), Some(r0), Some(c1), Some(r1)) = (
                    value("author"),
                    value("body"),
                    number("c0"),
                    number("r0"),
                    number("c1"),
                    number("r1"),
                ) else {
                    continue;
                };
                out.push(CommentThread {
                    id,
                    start: (c0, r0),
                    end: (c1, r1),
                    author,
                    body,
                    resolved,
                    replies: Vec::new(),
                });
            }
        }
        // Replies are independently-addressable records under
        // the eager root. Read every conflicting branch.
        let mut seen_replies = std::collections::HashSet::new();
        for reply_map in Self::annotation_maps(doc, idx, "comment_replies") {
            for reply_id in doc.keys(&reply_map) {
                if !seen_replies.insert(reply_id.clone()) {
                    continue;
                }
                let Ok(Some((Value::Object(ObjType::Map), reply))) =
                    doc.get(&reply_map, reply_id.as_str())
                else {
                    continue;
                };
                let (Some(parent_id), Some(author), Some(body)) = (
                    read_str(doc, &reply, "parent_id"),
                    read_str(doc, &reply, "author"),
                    read_str(doc, &reply, "body"),
                ) else {
                    continue;
                };
                if let Some(thread) = out.iter_mut().find(|thread| thread.id == parent_id) {
                    thread.replies.push(CommentReply {
                        id: reply_id,
                        author,
                        body,
                    });
                }
            }
        }
        for thread in &mut out {
            thread.replies.sort_by(|a, b| a.id.cmp(&b.id));
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    fn copy_annotations(doc: &mut AutoCommit, from: usize, to: usize) {
        for chart in Self::charts(doc, from) {
            Self::put_chart(doc, to, &chart);
        }
        for thread in Self::comments(doc, from) {
            Self::put_comment(doc, to, &thread);
        }
    }

    /// Rebase every annotation anchor through a row/column structural edit.
    /// An inserted/deleted line never leaves a chart or comment pointing at a
    /// stale coordinate: deleting an anchor's edge line shrinks it, and only an
    /// anchor whose every cell was deleted is dropped.
    pub fn remap_annotation_anchors(doc: &mut AutoCommit, idx: usize, edit: GridEdit) {
        for mut chart in Self::charts(doc, idx) {
            let Some((start, end)) = edit.map_range(chart.start, chart.end) else {
                Self::delete_chart(doc, idx, &chart.id);
                continue;
            };
            chart.start = (start.0.min(end.0), start.1.min(end.1));
            chart.end = (start.0.max(end.0), start.1.max(end.1));
            Self::put_chart(doc, idx, &chart);
        }
        for mut thread in Self::comments(doc, idx) {
            let Some((start, end)) = edit.map_range(thread.start, thread.end) else {
                Self::delete_comment(doc, idx, &thread.id);
                continue;
            };
            thread.start = (start.0.min(end.0), start.1.min(end.1));
            thread.end = (start.0.max(end.0), start.1.max(end.1));
            Self::put_comment(doc, idx, &thread);
        }
    }

    /// Snapshot one sheet (raw inputs + formats + name) for rendering.
    /// `default_currency` seeds any stored currency format whose subtype is
    /// missing (legacy data) — fresh formats always carry their own.
    pub fn snapshot_sheet(
        doc: &mut AutoCommit,
        idx: usize,
        default_currency: Currency,
    ) -> SheetSnapshot {
        let mut snap = SheetSnapshot::default();
        let Some(sheet) = Self::sheet_map(doc, idx) else {
            return snap;
        };
        snap.name = read_str(doc, &sheet, "name").unwrap_or_default();
        let Ok(Some((Value::Object(ObjType::Map), cells))) = doc.get(&sheet, "cells") else {
            return snap;
        };
        for key in doc.keys(&cells) {
            let Some(addr) = Self::parse_cell_key(&key) else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), cell))) = doc.get(&cells, key.as_str())
            else {
                continue;
            };
            if let Some(s) = read_str(doc, &cell, "raw")
                && !s.is_empty()
            {
                snap.raw.insert(addr, s);
            }
            if let Some(s) = read_str(doc, &cell, "fmt") {
                let f = CellFormat::from_str_with(&s, default_currency);
                if f != CellFormat::General {
                    snap.fmt.insert(addr, f);
                }
            }
            if read_bool(doc, &cell, "b") == Some(true) {
                snap.bold.insert(addr);
            }
        }
        snap
    }
}

fn parse_date_style(s: &str) -> Option<DateStyle> {
    Some(match s {
        "ISO" => DateStyle::Iso,
        "US" => DateStyle::Us,
        "EU" => DateStyle::Eu,
        "Long" => DateStyle::Long,
        _ => return None,
    })
}
fn date_style_code(s: DateStyle) -> &'static str {
    match s {
        DateStyle::Iso => "ISO",
        DateStyle::Us => "US",
        DateStyle::Eu => "EU",
        DateStyle::Long => "Long",
    }
}
fn parse_time_style(s: &str) -> Option<TimeStyle> {
    Some(match s {
        "H24" => TimeStyle::H24,
        "H12" => TimeStyle::H12,
        _ => return None,
    })
}
fn time_style_code(s: TimeStyle) -> &'static str {
    match s {
        TimeStyle::H24 => "H24",
        TimeStyle::H12 => "H12",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ActorId;
    use tilo_automerge::ApplyIncoming;

    /// Seed/snapshot round-trip, clearing cells and document settings.
    #[test]
    fn doc_cells_and_settings_round_trip() {
        // Case `seed_and_snapshot_round_trip`.
        {
            let mut doc = AutoCommit::new();
            let idx = SheetsDoc::add_sheet(&mut doc, "Budget");
            SheetsDoc::set_cell_raw(&mut doc, idx, (0, 0), "Hello");
            SheetsDoc::set_cell_raw(&mut doc, idx, (1, 0), "=1+1");
            SheetsDoc::set_cell_format(&mut doc, idx, (1, 0), CellFormat::Currency(Currency::USD));
            let snap = SheetsDoc::snapshot_sheet(&mut doc, idx, Currency::USD);
            assert_eq!(snap.name, "Budget");
            assert_eq!(snap.raw.get(&(0, 0)).unwrap(), "Hello");
            assert_eq!(snap.raw.get(&(1, 0)).unwrap(), "=1+1");
            assert_eq!(
                snap.fmt.get(&(1, 0)).copied(),
                Some(CellFormat::Currency(Currency::USD))
            );
        }

        // Case `clearing_cell_removes_it`.
        {
            let mut doc = AutoCommit::new();
            let idx = SheetsDoc::add_sheet(&mut doc, "S");
            SheetsDoc::set_cell_raw(&mut doc, idx, (0, 0), "x");
            SheetsDoc::set_cell_raw(&mut doc, idx, (0, 0), "");
            let snap = SheetsDoc::snapshot_sheet(&mut doc, idx, Currency::USD);
            assert!(snap.raw.is_empty());
        }

        // Case `doc_settings_round_trip_and_clear`.
        {
            let mut doc = AutoCommit::new();
            SheetsDoc::add_sheet(&mut doc, "S");
            // Default: everything inherits.
            assert_eq!(SheetsDoc::doc_settings(&mut doc), DocSettings::default());
            // Set an override.
            SheetsDoc::set_doc_settings(
                &mut doc,
                &DocSettings {
                    currency: Some(Currency::EUR),
                    date_format: Some(DateStyle::Eu),
                    time_format: None,
                },
            );
            let s = SheetsDoc::doc_settings(&mut doc);
            assert_eq!(s.currency, Some(Currency::EUR));
            assert_eq!(s.date_format, Some(DateStyle::Eu));
            assert_eq!(s.time_format, None);
            // Clearing returns to inherit.
            SheetsDoc::set_doc_settings(&mut doc, &DocSettings::default());
            assert_eq!(SheetsDoc::doc_settings(&mut doc), DocSettings::default());
        }
    }

    /// Multiple sheets, insert/delete, duplicate and move.
    #[test]
    fn sheet_structure_edits_keep_cells() {
        // Case `multiple_sheets`.
        {
            let mut doc = AutoCommit::new();
            SheetsDoc::add_sheet(&mut doc, "One");
            SheetsDoc::add_sheet(&mut doc, "Two");
            assert_eq!(SheetsDoc::sheet_count(&mut doc), 2);
            assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["One", "Two"]);
        }

        // Case `insert_and_delete_sheets`.
        {
            let mut doc = AutoCommit::new();
            SheetsDoc::add_sheet(&mut doc, "A");
            SheetsDoc::add_sheet(&mut doc, "C");
            // Insert "B" between A and C.
            SheetsDoc::insert_sheet(&mut doc, 1, "B");
            assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["A", "B", "C"]);
            // Delete the middle one.
            assert!(SheetsDoc::delete_sheet(&mut doc, 1));
            assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["A", "C"]);
            // The last remaining sheet cannot be deleted away.
            SheetsDoc::delete_sheet(&mut doc, 0);
            assert!(!SheetsDoc::delete_sheet(&mut doc, 0) || SheetsDoc::sheet_count(&mut doc) >= 1);
            assert!(
                SheetsDoc::sheet_count(&mut doc) >= 1,
                "a workbook keeps one sheet"
            );
        }

        // Case `duplicate_copies_cells_and_format`.
        {
            let mut doc = AutoCommit::new();
            let a = SheetsDoc::add_sheet(&mut doc, "Data");
            SheetsDoc::set_cell_raw(&mut doc, a, (0, 0), "=1+1");
            SheetsDoc::set_cell_format(&mut doc, a, (0, 0), CellFormat::Currency(Currency::EUR));
            SheetsDoc::set_sheet_color(&mut doc, a, Some(3));
            let dup = SheetsDoc::duplicate_sheet(&mut doc, a).expect("duplicated");
            assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["Data", "Data copy"]);
            assert_eq!(SheetsDoc::cell_raw(&mut doc, dup, (0, 0)), "=1+1");
            let snap = SheetsDoc::snapshot_sheet(&mut doc, dup, Currency::USD);
            assert_eq!(
                snap.fmt.get(&(0, 0)).copied(),
                Some(CellFormat::Currency(Currency::EUR))
            );
            assert_eq!(SheetsDoc::sheet_color(&mut doc, dup), Some(3));
        }

        // Case `move_sheet_reorders_and_keeps_cells`.
        {
            let mut doc = AutoCommit::new();
            let a = SheetsDoc::add_sheet(&mut doc, "A");
            SheetsDoc::add_sheet(&mut doc, "B");
            SheetsDoc::add_sheet(&mut doc, "C");
            let stable = SheetsDoc::sheet_resource_id(&doc, a).expect("sheet identity");
            SheetsDoc::set_cell_raw(&mut doc, a, (0, 0), "keep");
            // Move A (idx 0) to the right (idx 1): order becomes B, A, C.
            SheetsDoc::move_sheet(&mut doc, 0, 1);
            assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["B", "A", "C"]);
            // A's content survived the move.
            assert_eq!(SheetsDoc::cell_raw(&mut doc, 1, (0, 0)), "keep");
            assert_eq!(SheetsDoc::sheet_resource_id(&doc, 1), Some(stable));
        }
    }

    /// Charts/threads records, concurrent annotations, deletes and structural anchors.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn annotations_are_durable_and_merge_deterministically() {
        // Case `charts_and_threads_are_durable_sheet_records`.
        {
            let mut doc = AutoCommit::new();
            let sheet = SheetsDoc::add_sheet(&mut doc, "Data");
            let chart = ChartSnapshot {
                id: "2cc2d8ba-bc65-4e98-966a-7d0ecb4ac00e".into(),
                title: "Trend".into(),
                kind: ChartKind::Line,
                start: (0, 0),
                end: (2, 4),
            };
            SheetsDoc::put_chart(&mut doc, sheet, &chart);
            assert_eq!(SheetsDoc::charts(&mut doc, sheet), vec![chart]);
            let thread = CommentThread {
                id: "f9448d08-3b4a-4a7a-8e60-5259f8b67454".into(),
                start: (1, 1),
                end: (2, 1),
                author: "Alex".into(),
                body: "Review this".into(),
                resolved: false,
                replies: vec![CommentReply {
                    id: "4eb25e43-28e8-4255-98f4-8bd5b89b2f26".into(),
                    author: "Sam".into(),
                    body: "On it".into(),
                }],
            };
            SheetsDoc::put_comment(&mut doc, sheet, &thread);
            SheetsDoc::set_comment_resolved(&mut doc, sheet, &thread.id, true);
            let threads = SheetsDoc::comments(&mut doc, sheet);
            assert_eq!(threads[0].replies, thread.replies);
            assert!(threads[0].resolved);
        }

        // Case `concurrent_first_annotations_and_replies_survive_merge_and_reload`.
        {
            let mut a = AutoCommit::new();
            let sheet = SheetsDoc::add_sheet(&mut a, "Data");
            let thread = CommentThread {
                id: "00000000-0000-4000-8000-000000000003".into(),
                start: (0, 0),
                end: (0, 0),
                author: "Alex".into(),
                body: "Review".into(),
                resolved: false,
                replies: Vec::new(),
            };
            SheetsDoc::put_comment(&mut a, sheet, &thread);
            a.commit();
            let bytes = a.save();
            let mut b = AutoCommit::load(&bytes).expect("base loads");
            b.set_actor(ActorId::random());
            let chart_a = ChartSnapshot {
                id: "00000000-0000-4000-8000-000000000001".into(),
                title: "A".into(),
                kind: ChartKind::Column,
                start: (0, 0),
                end: (1, 1),
            };
            let chart_b = ChartSnapshot {
                id: "00000000-0000-4000-8000-000000000002".into(),
                title: "B".into(),
                kind: ChartKind::Line,
                start: (2, 0),
                end: (3, 1),
            };
            let comment_a = CommentThread {
                id: "00000000-0000-4000-8000-000000000006".into(),
                start: (1, 0),
                end: (1, 0),
                author: "A".into(),
                body: "First".into(),
                resolved: false,
                replies: Vec::new(),
            };
            let comment_b = CommentThread {
                id: "00000000-0000-4000-8000-000000000007".into(),
                start: (2, 0),
                end: (2, 0),
                author: "B".into(),
                body: "Second".into(),
                resolved: false,
                replies: Vec::new(),
            };
            SheetsDoc::put_chart(&mut a, sheet, &chart_a);
            SheetsDoc::put_comment(&mut a, sheet, &comment_a);
            SheetsDoc::put_chart(&mut b, sheet, &chart_b);
            SheetsDoc::put_comment(&mut b, sheet, &comment_b);
            SheetsDoc::put_comment_reply(
                &mut b,
                sheet,
                &thread.id,
                &CommentReply {
                    id: "00000000-0000-4000-8000-000000000004".into(),
                    author: "Sam".into(),
                    body: "Looking".into(),
                },
            );
            SheetsDoc::put_comment_reply(
                &mut a,
                sheet,
                &thread.id,
                &CommentReply {
                    id: "00000000-0000-4000-8000-000000000005".into(),
                    author: "Jo".into(),
                    body: "Approved".into(),
                },
            );
            a.merge_incoming(&mut b)
                .expect("merge concurrent first writes");
            let reloaded = AutoCommit::load(&a.save()).expect("final reload");
            let mut reloaded = reloaded;
            assert_eq!(SheetsDoc::charts(&mut reloaded, sheet).len(), 2);
            let threads = SheetsDoc::comments(&mut reloaded, sheet);
            assert_eq!(threads.len(), 3);
            assert_eq!(
                threads
                    .iter()
                    .find(|candidate| candidate.id == thread.id)
                    .expect("base thread")
                    .replies
                    .len(),
                2
            );
        }

        // Case `concurrent_comment_delete_hides_reply_and_resolution_deterministically`.
        {
            let mut a = AutoCommit::new();
            let sheet = SheetsDoc::add_sheet(&mut a, "Data");
            let thread = CommentThread {
                id: "00000000-0000-4000-8000-000000000010".into(),
                start: (0, 0),
                end: (0, 0),
                author: "Alex".into(),
                body: "Delete race".into(),
                resolved: false,
                replies: Vec::new(),
            };
            SheetsDoc::put_comment(&mut a, sheet, &thread);
            a.commit();
            let mut b = AutoCommit::load(&a.save()).expect("base loads");
            b.set_actor(ActorId::random());
            SheetsDoc::delete_comment(&mut a, sheet, &thread.id);
            SheetsDoc::set_comment_resolved(&mut b, sheet, &thread.id, true);
            SheetsDoc::put_comment_reply(
                &mut b,
                sheet,
                &thread.id,
                &CommentReply {
                    id: "00000000-0000-4000-8000-000000000011".into(),
                    author: "Sam".into(),
                    body: "Too late".into(),
                },
            );
            a.merge_incoming(&mut b).expect("merge");
            let mut reloaded = AutoCommit::load(&a.save()).expect("reload");
            assert!(
                SheetsDoc::comments(&mut reloaded, sheet).is_empty(),
                "delete wins; orphan replies are never surfaced"
            );
        }

        // Case `annotation_anchors_follow_structural_coordinate_changes`.
        {
            let mut doc = AutoCommit::new();
            let sheet = SheetsDoc::add_sheet(&mut doc, "Data");
            let chart = ChartSnapshot {
                id: "00000000-0000-4000-8000-000000000020".into(),
                title: "Trend".into(),
                kind: ChartKind::Line,
                start: (1, 1),
                end: (3, 3),
            };
            let thread = CommentThread {
                id: "00000000-0000-4000-8000-000000000021".into(),
                start: (2, 2),
                end: (2, 2),
                author: "Alex".into(),
                body: "Follow".into(),
                resolved: false,
                replies: Vec::new(),
            };
            SheetsDoc::put_chart(&mut doc, sheet, &chart);
            SheetsDoc::put_comment(&mut doc, sheet, &thread);
            SheetsDoc::remap_annotation_anchors(&mut doc, sheet, GridEdit::InsertRow(0));
            assert_eq!(SheetsDoc::charts(&mut doc, sheet)[0].start, (1, 2));
            assert_eq!(SheetsDoc::comments(&mut doc, sheet)[0].start, (2, 3));
            // The chart spans B3:D5 and the comment sits on C4. Deleting the
            // chart's first row, then its last, shrinks it instead of deleting it.
            SheetsDoc::remap_annotation_anchors(&mut doc, sheet, GridEdit::DeleteRow(2));
            let chart = &SheetsDoc::charts(&mut doc, sheet)[0];
            assert_eq!((chart.start, chart.end), ((1, 2), (3, 3)));
            assert_eq!(SheetsDoc::comments(&mut doc, sheet)[0].start, (2, 2));
            SheetsDoc::remap_annotation_anchors(&mut doc, sheet, GridEdit::DeleteRow(3));
            let chart = &SheetsDoc::charts(&mut doc, sheet)[0];
            assert_eq!((chart.start, chart.end), ((1, 2), (3, 2)));
            SheetsDoc::remap_annotation_anchors(&mut doc, sheet, GridEdit::DeleteColumn(2));
            assert!(
                SheetsDoc::comments(&mut doc, sheet).is_empty(),
                "a fully deleted one-cell anchor removes its thread"
            );
            SheetsDoc::remap_annotation_anchors(&mut doc, sheet, GridEdit::DeleteColumn(1));
            let chart = &SheetsDoc::charts(&mut doc, sheet)[0];
            assert_eq!((chart.start, chart.end), ((1, 2), (1, 2)));
            SheetsDoc::remap_annotation_anchors(&mut doc, sheet, GridEdit::DeleteRow(2));
            assert!(SheetsDoc::charts(&mut doc, sheet).is_empty());
        }
    }
}
