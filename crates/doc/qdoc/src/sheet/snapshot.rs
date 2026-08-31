//! Read-only materialized view of a [`crate::sheet::Workbook`] for rendering,
//! export, and diffing.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The value type of a cell's stored/cached value; orthogonal to whether it's a
/// formula (a formula cell still carries a `cell_type` for its cached result).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CellType {
    #[default]
    Empty,
    Number,
    Text,
    Bool,
    Error,
}

impl CellType {
    /// The canonical schema string.
    pub fn as_str(self) -> &'static str {
        use crate::sheet::schema::{
            CELL_TYPE_BOOL, CELL_TYPE_EMPTY, CELL_TYPE_ERROR, CELL_TYPE_NUMBER, CELL_TYPE_TEXT,
        };
        match self {
            CellType::Empty => CELL_TYPE_EMPTY,
            CellType::Number => CELL_TYPE_NUMBER,
            CellType::Text => CELL_TYPE_TEXT,
            CellType::Bool => CELL_TYPE_BOOL,
            CellType::Error => CELL_TYPE_ERROR,
        }
    }
    /// Parses a schema string back into a [`CellType`] (unknown → `Text`, the safe default).
    #[allow(
        clippy::should_implement_trait,
        reason = "returns Self infallibly by design, not std::str::FromStr's fallible contract"
    )]
    pub fn from_str(s: &str) -> Self {
        use crate::sheet::schema::{
            CELL_TYPE_BOOL, CELL_TYPE_EMPTY, CELL_TYPE_ERROR, CELL_TYPE_NUMBER,
        };
        match s {
            CELL_TYPE_EMPTY => CellType::Empty,
            CELL_TYPE_NUMBER => CellType::Number,
            CELL_TYPE_BOOL => CellType::Bool,
            CELL_TYPE_ERROR => CellType::Error,
            _ => CellType::Text,
        }
    }
}

/// One materialized cell: its A1 reference + coordinates, stored value and type, preserved formula, and opaque style ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellView {
    /// A1 reference, e.g. `"B7"`.
    pub reference: String,
    /// 1-based column (`A` = 1).
    pub col: u32,
    /// 1-based row.
    pub row: u32,
    /// The literal / cached value as text (interpreted per `cell_type`).
    pub value: String,
    pub cell_type: CellType,
    /// The formula source (without the leading `=`), preserved verbatim; `None` for a plain value cell.
    #[serde(default)]
    pub formula: Option<String>,
    /// Opaque style reference carried through the round-trip. `None` if unstyled.
    #[serde(default)]
    pub style: Option<String>,
}

/// One materialized sheet: id, tab name, and its cells sorted by `(row, col)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetView {
    pub id: String,
    pub name: String,
    /// Cells in row-major order (`(row, col)` ascending); sparse — only cells carrying a value, formula, or style are present.
    pub cells: Vec<CellView>,
}

impl SheetView {
    /// The cell at an A1 reference, if present.
    pub fn cell(&self, reference: &str) -> Option<&CellView> {
        self.cells.iter().find(|c| c.reference == reference)
    }
}

/// The whole workbook materialized: schema id/version, ordered sheets, and document properties.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkbookSnapshot {
    pub schema: String,
    pub schema_version: i64,
    /// Sheets in tab order.
    pub sheets: Vec<SheetView>,
    /// Document properties (`title`/`author`/… → value).
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

impl WorkbookSnapshot {
    /// The sheet with the given id, if present.
    pub fn sheet(&self, id: &str) -> Option<&SheetView> {
        self.sheets.iter().find(|s| s.id == id)
    }
    /// The sheet with the given tab name, if present.
    pub fn sheet_by_name(&self, name: &str) -> Option<&SheetView> {
        self.sheets.iter().find(|s| s.name == name)
    }
}
