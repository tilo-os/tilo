//! Sheets — a spreadsheet for qOS, built on the `ui` toolkit.
//!
//! Layers:
//!   * [`model`]    — A1 addressing, cell values, number/date/time/currency
//!     formats with per-currency symbols and date/time styles.
//!   * [`infer`]    — smart type inference on entry (Excel/Sheets-style: a
//!     trailing `%` becomes a percent, a currency symbol becomes money, a
//!     parseable date/time becomes a date/time, …).
//!   * [`formula`]  — tokenizer -> parser -> evaluator with ranges, common
//!     functions, dependency-aware recalculation, and cycle detection, plus
//!     unit/format propagation for computed cells.
//!   * [`settings`] — the shared per-user global settings store and the
//!     per-document overrides that layer on top of it.
//!   * [`doc`]      — the Automerge document model (sheets + cells live in
//!     the CRDT, so collaboration and undo come for free).
//!   * [`csv`]      — CSV import/export.
//!   * [`xlsx`]     — Excel `.xlsx` import/export.
//!   * [`ods`]      — LibreOffice Calc `.ods` import/export.
//!   * [`grid`]     — the hand-rolled scrollable, editable cell grid with a
//!     floating in-cell editor that overflows into empty neighbours.
//!   * [`app`]      — the root widget composing the formula bar + grid +
//!     sheet tabs + the file/format menus and settings surfaces.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

pub mod actions;
pub mod app;
pub mod csv;
pub mod doc;
pub mod formula;
pub mod grid;
pub mod infer;
pub mod model;
pub mod ods;
pub mod settings;
pub mod xlsx;

pub use app::SheetsApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(SheetsApp::new())
}
