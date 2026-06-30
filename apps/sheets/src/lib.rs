//! Sheets — a CRDT-native spreadsheet for qOS, built on the `ui` toolkit.
//!
//! Layers:
//!   * [`model`]  — A1 addressing, cell values, number/date/currency formats.
//!   * [`formula`] — tokenizer -> parser -> evaluator with ranges, common
//!     functions, dependency-aware recalculation, and cycle detection.
//!   * [`doc`]    — the Automerge document model (sheets + cells live in the
//!     CRDT, so collaboration and undo come for free).
//!   * [`csv`]    — CSV import/export.
//!   * [`grid`]   — the hand-rolled scrollable, editable cell grid.
//!   * [`app`]    — the root widget composing toolbar + formula bar + grid +
//!     sheet tabs.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

pub mod app;
pub mod csv;
pub mod doc;
pub mod formula;
pub mod grid;
pub mod model;

pub use app::SheetsApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(SheetsApp::new())
}
