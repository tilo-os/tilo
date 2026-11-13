//! CRDT-native spreadsheet model: a workbook is an ordered set of sheets, each a
//! sparse map of A1-keyed cells carrying a value, its type, a preserved formula and
//! an opaque style ref. It is a separate schema (`tilo.sheet`) backed by automerge,
//! so a workbook syncs, versions and merges like the rich-text document.

pub mod a1;
pub mod model;
pub mod schema;
pub mod snapshot;

pub use model::{CellInput, SheetContainers, Workbook};
pub use snapshot::{CellType, CellView, SheetView, WorkbookSnapshot};
