//! CRDT-native spreadsheet model: a workbook is an ordered set of sheets, each a
//! sparse map of A1-keyed cells carrying value + type + **formula** (preserved,
//! never flattened to its result) + an opaque style ref. Backed by automerge so a
//! workbook syncs, versions, and merges exactly like the rich-text
//! [`crate::Document`] — a separate schema (`qos.sheet`) for a separate shape.
//!
//! The `.xlsx` / `.ods` codecs (see [`crate::codecs`]) parse untrusted spreadsheet
//! packages into this model through the same bounded/hardened ZIP + XML entry as
//! the document codecs ([`crate::codecs::safety`]), and serialize it back.

pub mod a1;
pub mod model;
pub mod schema;
pub mod snapshot;

pub use model::{CellInput, SheetContainers, Workbook};
pub use snapshot::{CellType, CellView, SheetView, WorkbookSnapshot};
