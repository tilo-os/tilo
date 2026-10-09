//! Diff — a CRDT-native diff & merge tool for Tilo, built on the `ui` toolkit.
//!
//! Two modes:
//!
//! * **Diff** — a side-by-side, line-aligned comparison of two files with
//!   syntax highlighting, add/remove/modify tinting, a change minimap, and
//!   prev/next-change navigation.
//! * **3-way merge** — base / ours / theirs source columns with per-hunk
//!   conflict resolution (take ours / theirs / both) and an editable merged
//!   result, a text in the document every collaborator edits.
//!
//! What is open and the merged result are the [`Diffmerge`] resource;
//! [`DiffmergeShell`] runs it.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints remain denied; shared crates retain the full lint set
// because a fault there can affect the session or every app.
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
mod chrome;
pub mod diff_view;
pub mod doc;
pub mod highlight;
pub mod i18n;
pub mod merge_view;
pub mod model;
mod pane;
pub mod resource;
pub mod samples;
mod scroll;

pub use actions::DiffmergeEffect;
pub use app::{DiffMergeApp, Mode};
pub use doc::{HunkChoice, Session};
pub use model::{DiffModel, MergeModel, Resolution};
pub use resource::DiffmergeShell;

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<DiffmergeShell>()
}
