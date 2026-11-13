//! The canonical spreadsheet automerge layout (schema v1). Public ABI: the
//! spreadsheet codecs and the workbook snapshot depend on these field names. A
//! workbook is a separate CRDT schema from the rich-text [`crate::schema`]: `sheets
//! : Map<sheet_id, sheet>`, a sheet's `order` (fractional tab key), `name` and
//! `cells : Map<cell_ref, cell>`, a cell's `value`, `type`, optional `formula`
//! (source without the leading `=`) and `style`, plus `meta` with `props` and
//! `preserved` parts. Cells keyed by A1 reference is the CRDT-friendly shape: two
//! replicas editing different cells touch different keys and merge, and edits to
//! one cell resolve field by field.

pub const SHEET_SCHEMA_ID: &str = "tilo.sheet";
pub const SHEET_SCHEMA_VERSION: u64 = 1;

pub const KEY_SHEET_SCHEMA: &str = "schema";
pub const KEY_SHEET_SCHEMA_VERSION: &str = tilo_resource::evolution::VERSION_KEY;
pub const KEY_SHEETS: &str = "sheets";
pub const KEY_SHEET_META: &str = "meta";
pub const KEY_SHEET_PRESERVED: &str = "preserved";
pub const KEY_SHEET_PROPS: &str = "props";

pub const SHEET_ORDER: &str = "order";
pub const SHEET_NAME: &str = "name";
pub const SHEET_CELLS: &str = "cells";

pub const CELL_VALUE: &str = "value";
pub const CELL_TYPE: &str = "type";
pub const CELL_FORMULA: &str = "formula";
pub const CELL_STYLE: &str = "style";

pub const CELL_TYPE_EMPTY: &str = "empty";
pub const CELL_TYPE_NUMBER: &str = "number";
pub const CELL_TYPE_TEXT: &str = "text";
pub const CELL_TYPE_BOOL: &str = "bool";
pub const CELL_TYPE_ERROR: &str = "error";

// Well-known document-property names, shared with the rich-text codecs' meta.
pub const SHEET_PROP_TITLE: &str = "title";
pub const SHEET_PROP_AUTHOR: &str = "author";
pub const SHEET_PROP_CREATED: &str = "created";
pub const SHEET_PROP_MODIFIED: &str = "modified";
pub const SHEET_PROP_GENERATOR: &str = "generator";
