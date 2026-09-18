//! The canonical spreadsheet automerge layout (schema v1). Public ABI: the
//! spreadsheet codecs and the workbook snapshot depend on these field names.
//!
//! A workbook is a separate CRDT schema from the rich-text [`crate::schema`]:
//!
//! ```text
//! root
//!   schema          = "tilo.sheet"
//!   schema_version  = 1
//!   sheets : Map<sheet_id, sheet>
//!     sheet
//!       order : Bytes   (fractional tab-order key — sheets sort by this)
//!       name  : Str
//!       cells : Map<cell_ref, cell>          (sparse, keyed by "A1"-style ref)
//!         cell
//!           value   : Str   (the literal / cached value)
//!           type    : Str   ("number"|"text"|"bool"|"error"|"empty")
//!           formula : Str   (formula source without the leading '='), optional
//!           style   : Str   (opaque style ref carried through), optional
//!   meta      : Map          (document properties live under `props`)
//!     props   : Map<prop_name, Str>
//!   preserved : Map<part_name, Bytes>   (verbatim unmodeled package parts)
//! ```
//!
//! Cells keyed by their A1 reference is the CRDT-friendly shape: two replicas
//! editing *different* cells touch different map keys and merge; edits to the same
//! cell resolve field-by-field (value vs. formula vs. style merge independently).

pub const SHEET_SCHEMA_ID: &str = "tilo.sheet";
pub const SHEET_SCHEMA_VERSION: i64 = 1;

pub const KEY_SHEET_SCHEMA: &str = "schema";
pub const KEY_SHEET_SCHEMA_VERSION: &str = "schema_version";
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

// Well-known document-property names (shared with the rich-text codecs' meta).
pub const SHEET_PROP_TITLE: &str = "title";
pub const SHEET_PROP_AUTHOR: &str = "author";
pub const SHEET_PROP_CREATED: &str = "created";
pub const SHEET_PROP_MODIFIED: &str = "modified";
pub const SHEET_PROP_GENERATOR: &str = "generator";
