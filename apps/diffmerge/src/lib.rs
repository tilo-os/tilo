//! Diff: a side-by-side, line-aligned comparison of two texts with syntax highlighting, a
//! change minimap and prev/next navigation, and a three-way merge with per-hunk conflict
//! resolution and an editable merged result. What is open and the merged result are the
//! [`Session`] resource.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
// Panic-producing lints stay denied; shared crates keep the full set.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
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
pub mod folder;
mod folder_view;
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

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(DiffmergeShell)
}
