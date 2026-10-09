//! Sheets — a spreadsheet for Tilo, built on the `ui` toolkit.
//!
//! Layers:
//!   * [`model`]    — A1 addressing, cell values, number/date/time/currency
//!     formats with per-currency symbols and date/time styles.
//!   * [`infer`]    — smart type inference on entry (Excel/Sheets-style: a
//!     trailing `%` becomes a percent, a currency symbol becomes money, a
//!     parseable date/time becomes a date/time, …).
//!   * [`formula`]  — tokenizer -> parser -> evaluator with ranges, common
//!     functions, dependency-aware recalculation, and cycle detection, plus
//!     unit/format propagation for computed cells, and reference adjustment
//!     (relative/absolute) for copy/paste and fill.
//!   * [`fill`]     — the smart fill-handle engine: extend a selected pattern
//!     (formula, numeric series, dates, month/weekday names, text+number).
//!   * [`settings`] — the person's shared locale defaults and the
//!     per-document overrides that layer on top of it.
//!   * [`doc`]      — the Automerge document model (sheets + cells live in
//!     the CRDT, so collaboration and undo come for free).
//!   * [`csv`]      — CSV import/export.
//!   * [`xlsx`]     — Excel `.xlsx` import/export.
//!   * [`ods`]      — `LibreOffice` Calc `.ods` import/export.
//!   * [`grid`]     — the scrollable, editable cell grid with a
//!     floating in-cell editor that overflows into empty neighbours.
//!   * [`app`]      — the root widget composing the formula bar + grid +
//!     sheet tabs + the file/format menus and settings surfaces.
//!
//! [`SheetsShell`] is the app every host runs: the live window, screenshots
//! and the browser desktop.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod cell_actions;
pub mod csv;
pub mod doc;
pub mod fill;
pub mod formula;
pub mod grid;
pub mod infer;
#[cfg(test)]
mod interchange_tests;
pub mod model;
pub mod ods;
pub mod resource;
pub mod sample;
pub mod settings;
pub mod xlsx;

pub use app::SheetsApp;

pub use resource::{SheetsShell, SheetsWorkbook};

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<SheetsShell>()
}
