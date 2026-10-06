//! CRDT-native spreadsheet model: a workbook is an ordered set of sheets, each a sparse map of
//! A1-keyed cells carrying value + type + **formula** (preserved, never flattened to its result) +
//! an opaque style ref. Backed by automerge so a workbook syncs, versions, and merges like the
//! rich-text [`crate::Document`] — a separate schema (`tilo.sheet`) for a separate shape. The
//! `.xlsx`/`.ods` codecs parse untrusted packages into this model through the same
//! bounded/hardened ZIP + XML entry as the document codecs (`crate::codecs::safety`).

pub mod a1;
pub mod model;
pub mod resource;
pub mod schema;
pub mod snapshot;

pub use model::{CellInput, SheetContainers, Workbook};
pub use resource::{WorkbookAction, WorkbookDoc};
pub use snapshot::{CellType, CellView, SheetView, WorkbookSnapshot};
