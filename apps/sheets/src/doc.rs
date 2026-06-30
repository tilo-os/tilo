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
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, Value, ROOT};

use crate::model::{Addr, CellFormat};

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

    /// Get-or-create the top-level `wb` Map.
    pub fn wb_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "wb") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "wb", ObjType::Map)
                .expect("put wb never fails"),
        }
    }

    /// Get-or-create the `sheets` List under `wb`.
    pub fn sheets_list(doc: &mut AutoCommit) -> ObjId {
        let wb = Self::wb_map(doc);
        match doc.get(&wb, "sheets") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => doc
                .put_object(&wb, "sheets", ObjType::List)
                .expect("put sheets never fails"),
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

    /// The Map ObjId for sheet `idx`, if it exists.
    fn sheet_map(doc: &mut AutoCommit, idx: usize) -> Option<ObjId> {
        let sheets = Self::sheets_list(doc);
        match doc.get(&sheets, idx) {
            Ok(Some((Value::Object(ObjType::Map), id))) => Some(id),
            _ => None,
        }
    }

    /// The `cells` Map for sheet `idx`, creating it if missing.
    fn cells_map(doc: &mut AutoCommit, idx: usize) -> Option<ObjId> {
        let sheet = Self::sheet_map(doc, idx)?;
        Some(match doc.get(&sheet, "cells") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&sheet, "cells", ObjType::Map)
                .expect("put cells never fails"),
        })
    }

    /// Append a new sheet with `name` and return its index.
    pub fn add_sheet(doc: &mut AutoCommit, name: &str) -> usize {
        let sheets = Self::sheets_list(doc);
        let idx = doc.length(&sheets);
        let sheet = doc
            .insert_object(&sheets, idx, ObjType::Map)
            .expect("insert sheet map never fails");
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

    /// All sheet names in order.
    pub fn sheet_names(doc: &mut AutoCommit) -> Vec<String> {
        let sheets = Self::sheets_list(doc);
        let n = doc.length(&sheets);
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let name = match doc.get(&sheets, i) {
                Ok(Some((Value::Object(ObjType::Map), sheet))) => {
                    match doc.get(&sheet, "name") {
                        Ok(Some((Value::Scalar(v), _))) => {
                            v.to_str().map(|s| s.to_string()).unwrap_or_default()
                        }
                        _ => String::new(),
                    }
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
            if let Ok(Some((Value::Object(ObjType::Map), cell))) =
                doc.get(&cells, key.as_str())
            {
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
            _ => doc
                .put_object(&cells, key.as_str(), ObjType::Map)
                .expect("put cell map never fails"),
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
            _ => doc
                .put_object(&cells, key.as_str(), ObjType::Map)
                .expect("put cell map never fails"),
        };
        let _ = doc.put(&cell, "fmt", fmt.as_str());
    }

    /// Read the raw input of a single cell.
    pub fn cell_raw(doc: &mut AutoCommit, idx: usize, addr: Addr) -> String {
        let Some(cells) = Self::cells_map(doc, idx) else {
            return String::new();
        };
        let key = Self::cell_key(addr);
        match doc.get(&cells, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), cell))) => match doc.get(&cell, "raw") {
                Ok(Some((Value::Scalar(v), _))) => {
                    v.to_str().map(|s| s.to_string()).unwrap_or_default()
                }
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    /// Snapshot one sheet (raw inputs + formats + name) for rendering.
    pub fn snapshot_sheet(doc: &mut AutoCommit, idx: usize) -> SheetSnapshot {
        let mut snap = SheetSnapshot::default();
        let Some(sheet) = Self::sheet_map(doc, idx) else {
            return snap;
        };
        snap.name = match doc.get(&sheet, "name") {
            Ok(Some((Value::Scalar(v), _))) => {
                v.to_str().map(|s| s.to_string()).unwrap_or_default()
            }
            _ => String::new(),
        };
        let cells = match doc.get(&sheet, "cells") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return snap,
        };
        for key in doc.keys(&cells) {
            let Some(addr) = Self::parse_cell_key(&key) else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), cell))) =
                doc.get(&cells, key.as_str())
            else {
                continue;
            };
            if let Ok(Some((Value::Scalar(v), _))) = doc.get(&cell, "raw") {
                if let Some(s) = v.to_str() {
                    if !s.is_empty() {
                        snap.raw.insert(addr, s.to_string());
                    }
                }
            }
            if let Ok(Some((Value::Scalar(v), _))) = doc.get(&cell, "fmt") {
                if let Some(s) = v.to_str() {
                    let f = CellFormat::from_str(s);
                    if f != CellFormat::General {
                        snap.fmt.insert(addr, f);
                    }
                }
            }
        }
        snap
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
        SheetsDoc::set_cell_format(&mut doc, idx, (1, 0), CellFormat::Currency);
        let snap = SheetsDoc::snapshot_sheet(&mut doc, idx);
        assert_eq!(snap.name, "Budget");
        assert_eq!(snap.raw.get(&(0, 0)).unwrap(), "Hello");
        assert_eq!(snap.raw.get(&(1, 0)).unwrap(), "=1+1");
        assert_eq!(snap.fmt.get(&(1, 0)).copied(), Some(CellFormat::Currency));
    }

    #[test]
    fn clearing_cell_removes_it() {
        let mut doc = AutoCommit::new();
        let idx = SheetsDoc::add_sheet(&mut doc, "S");
        SheetsDoc::set_cell_raw(&mut doc, idx, (0, 0), "x");
        SheetsDoc::set_cell_raw(&mut doc, idx, (0, 0), "");
        let snap = SheetsDoc::snapshot_sheet(&mut doc, idx);
        assert!(snap.raw.is_empty());
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
}
