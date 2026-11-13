//! Read-only materialized view of a [`crate::sheet::Workbook`] for rendering,
//! export, and diffing.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The value type of a cell's stored or cached value, orthogonal to whether it is a formula (a
/// formula cell still carries a type for its cached result).
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
    /// Parse a schema string back into a [`CellType`], unknown → `Text`.
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellView {
    pub reference: String,
    pub col: u32,
    pub row: u32,
    pub value: String,
    pub cell_type: CellType,
    /// The formula source (without the leading `=`), preserved verbatim; `None` for a plain value
    /// cell.
    #[serde(default)]
    pub formula: Option<String>,
    #[serde(default)]
    pub style: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetView {
    pub id: String,
    pub name: String,
    /// Cells in row-major order, sparse: only cells carrying a value, formula or style are present.
    pub cells: Vec<CellView>,
    /// Whether the tab is hidden (an `.xlsx` writes `state="hidden"`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
}

impl SheetView {
    pub fn cell(&self, reference: &str) -> Option<&CellView> {
        self.cells.iter().find(|c| c.reference == reference)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkbookSnapshot {
    pub schema: String,
    pub schema_version: u64,
    pub sheets: Vec<SheetView>,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

impl WorkbookSnapshot {
    pub fn sheet(&self, id: &str) -> Option<&SheetView> {
        self.sheets.iter().find(|s| s.id == id)
    }
    pub fn sheet_by_name(&self, name: &str) -> Option<&SheetView> {
        self.sheets.iter().find(|s| s.name == name)
    }
}
