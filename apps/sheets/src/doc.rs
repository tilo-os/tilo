//! Automerge document model for the workbook.
//!
//! State lives in the CRDT (`ui::State::doc`), so two peers editing the
//! same workbook converge for free. Layout:
//!
//! ```text
//! root
//! └── "wb" (Map)
//!     ├── "active" (Int)              # active sheet index
//!     └── "sheets" (List)
//!          └── [i] (Map)
//!              ├── "name"  (Str)
//!              └── "cells" (Map)      # key "col,row" -> Map
//!                   └── "<c>,<r>" (Map)
//!                        ├── "raw" (Str)   # raw input: "=SUM(A1:A3)" or "42"
//!                        └── "fmt" (Str)   # CellFormat::as_str()
//! ```
//!
//! Cells are keyed by a `"col,row"` string (zero-based) rather than A1 so
//! the key is cheap to build/parse and stable across column renames.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};

use crate::model::{Addr, CellFormat, Currency, DateStyle, TimeStyle};
use crate::settings::DocSettings;

/// One sheet's snapshot pulled out of the CRDT for rendering.
#[derive(Clone, Debug, Default)]
pub struct SheetSnapshot {
    pub name: String,
    /// raw input string per populated cell
    pub raw: std::collections::HashMap<Addr, String>,
    /// per-cell format (only present when not General)
    pub fmt: std::collections::HashMap<Addr, CellFormat>,
}

/// Stateless namespace of workbook document helpers.
pub struct SheetsDoc;

impl SheetsDoc {
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

    /// Whether the workbook has been seeded at all.
    pub fn is_empty(doc: &mut AutoCommit) -> bool {
        Self::sheet_count(doc) == 0
    }

    /// Active sheet index (clamped to a valid sheet when out of range).
    pub fn active_index(doc: &mut AutoCommit) -> usize {
        let wb = Self::wb_map(doc);
        let raw = match doc.get(&wb, "active") {
            Ok(Some((Value::Scalar(v), _))) => v.to_i64().unwrap_or(0),
            _ => 0,
        };
        let count = Self::sheet_count(doc).max(1);
        (raw.max(0) as usize).min(count - 1)
    }

    pub fn set_active_index(doc: &mut AutoCommit, idx: usize) {
        let wb = Self::wb_map(doc);
        let _ = doc.put(&wb, "active", idx as i64);
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
    /// Legacy sheets fall back to their Automerge object id; structural moves
    /// copy that value into the replacement map so identity survives index and
    /// name changes.
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
        match doc.get(&sheet, "resource_id") {
            Ok(Some((Value::Scalar(value), _))) => value
                .to_str()
                .map(str::to_string)
                .or_else(|| Some(sheet.to_string())),
            _ => Some(sheet.to_string()),
        }
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
        let sheets = Self::sheets_list(doc);
        let idx = doc.length(&sheets);
        let sheet = Self::insert_object_or_panic(doc, &sheets, idx, ObjType::Map);
        let _ = doc.put(&sheet, "resource_id", sheet.to_string());
        let _ = doc.put(&sheet, "name", name);
        let _ = doc.put_object(&sheet, "cells", ObjType::Map);
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
        let sheets = Self::sheets_list(doc);
        let at = at.min(doc.length(&sheets));
        let sheet = Self::insert_object_or_panic(doc, &sheets, at, ObjType::Map);
        let _ = doc.put(&sheet, "resource_id", sheet.to_string());
        let _ = doc.put(&sheet, "name", name);
        let _ = doc.put_object(&sheet, "cells", ObjType::Map);
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
        Self::sheet_map(doc, idx)?;
        let snap = Self::snapshot_sheet(doc, idx, Currency::USD);
        let color = Self::sheet_color(doc, idx);
        let new_name = format!("{} copy", snap.name);
        let new_idx = Self::insert_sheet(doc, idx + 1, &new_name);
        for (addr, raw) in &snap.raw {
            Self::set_cell_raw(doc, new_idx, *addr, raw);
        }
        for (addr, fmt) in &snap.fmt {
            Self::set_cell_format(doc, new_idx, *addr, *fmt);
        }
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
        let sheets = Self::sheets_list(doc);
        let _ = doc.delete(&sheets, from);
        let insert_at = to.min(len - 1);
        let sheet = Self::insert_object_or_panic(doc, &sheets, insert_at, ObjType::Map);
        if let Some(resource_id) = resource_id {
            let _ = doc.put(&sheet, "resource_id", resource_id);
        }
        let _ = doc.put(&sheet, "name", &snap.name);
        let _ = doc.put_object(&sheet, "cells", ObjType::Map);
        for (addr, raw) in &snap.raw {
            Self::set_cell_raw(doc, insert_at, *addr, raw);
        }
        for (addr, fmt) in &snap.fmt {
            Self::set_cell_format(doc, insert_at, *addr, *fmt);
        }
        if let Some(c) = color {
            Self::set_sheet_color(doc, insert_at, Some(c));
        }
    }

    /// A sheet's tab colour, stored as a palette index. `None` means no colour.
    pub fn sheet_color(doc: &mut AutoCommit, idx: usize) -> Option<u32> {
        let sheet = Self::sheet_map(doc, idx)?;
        match doc.get(&sheet, "color") {
            Ok(Some((Value::Scalar(v), _))) => v.to_i64().map(|n| n as u32),
            _ => None,
        }
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
                Ok(Some((Value::Object(ObjType::Map), sheet))) => match doc.get(&sheet, "name") {
                    Ok(Some((Value::Scalar(v), _))) => v
                        .to_str()
                        .map(std::string::ToString::to_string)
                        .unwrap_or_default(),
                    _ => String::new(),
                },
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
                let has_fmt = matches!(doc.get(&cell, "fmt"), Ok(Some(_)));
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

    /// Get-or-create the per-document `settings` Map under `wb`.
    fn settings_map(doc: &mut AutoCommit) -> ObjId {
        let wb = Self::wb_map(doc);
        match doc.get(&wb, "settings") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => Self::put_object_or_panic(doc, &wb, "settings", ObjType::Map),
        }
    }

    fn read_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => v.to_str().map(std::string::ToString::to_string),
            _ => None,
        }
    }

    /// Read the per-document locale overrides from the CRDT. Absent fields
    /// mean "inherit the global default".
    pub fn doc_settings(doc: &mut AutoCommit) -> DocSettings {
        let s = Self::settings_map(doc);
        DocSettings {
            currency: Self::read_str(doc, &s, "currency").and_then(|c| Currency::from_code(&c)),
            date_format: Self::read_str(doc, &s, "date_format").and_then(|v| parse_date_style(&v)),
            time_format: Self::read_str(doc, &s, "time_format").and_then(|v| parse_time_style(&v)),
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
            Ok(Some((Value::Object(ObjType::Map), cell))) => match doc.get(&cell, "raw") {
                Ok(Some((Value::Scalar(v), _))) => v
                    .to_str()
                    .map(std::string::ToString::to_string)
                    .unwrap_or_default(),
                _ => String::new(),
            },
            _ => String::new(),
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
        snap.name = match doc.get(&sheet, "name") {
            Ok(Some((Value::Scalar(v), _))) => v
                .to_str()
                .map(std::string::ToString::to_string)
                .unwrap_or_default(),
            _ => String::new(),
        };
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
            if let Ok(Some((Value::Scalar(v), _))) = doc.get(&cell, "raw")
                && let Some(s) = v.to_str()
                && !s.is_empty()
            {
                snap.raw.insert(addr, s.to_string());
            }
            if let Ok(Some((Value::Scalar(v), _))) = doc.get(&cell, "fmt")
                && let Some(s) = v.to_str()
            {
                let f = CellFormat::from_str_with(s, default_currency);
                if f != CellFormat::General {
                    snap.fmt.insert(addr, f);
                }
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

    #[test]
    fn seed_and_snapshot_round_trip() {
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

    #[test]
    fn clearing_cell_removes_it() {
        let mut doc = AutoCommit::new();
        let idx = SheetsDoc::add_sheet(&mut doc, "S");
        SheetsDoc::set_cell_raw(&mut doc, idx, (0, 0), "x");
        SheetsDoc::set_cell_raw(&mut doc, idx, (0, 0), "");
        let snap = SheetsDoc::snapshot_sheet(&mut doc, idx, Currency::USD);
        assert!(snap.raw.is_empty());
    }

    #[test]
    fn doc_settings_round_trip_and_clear() {
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

    #[test]
    fn multiple_sheets_and_active() {
        let mut doc = AutoCommit::new();
        SheetsDoc::add_sheet(&mut doc, "One");
        SheetsDoc::add_sheet(&mut doc, "Two");
        assert_eq!(SheetsDoc::sheet_count(&mut doc), 2);
        SheetsDoc::set_active_index(&mut doc, 1);
        assert_eq!(SheetsDoc::active_index(&mut doc), 1);
        assert_eq!(SheetsDoc::sheet_names(&mut doc), vec!["One", "Two"]);
    }

    #[test]
    fn insert_and_delete_sheets() {
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

    #[test]
    fn duplicate_copies_cells_and_format() {
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

    #[test]
    fn move_sheet_reorders_and_keeps_cells() {
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
