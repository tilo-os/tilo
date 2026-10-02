//! Diff — a CRDT-native diff & merge tool for Tilo, built on the `ui` toolkit.
//!
//! Two modes:
//!
//! * **Diff** — a side-by-side, line-aligned comparison of two files with
//!   syntax highlighting, add/remove/modify tinting, a change minimap, and
//!   prev/next-change navigation.
//! * **3-way merge** — base / ours / theirs source columns with per-hunk
//!   conflict resolution (take ours / theirs / both) and an editable merged
//!   result mirrored into an Automerge `Text` so the output is collaboratively
//!   editable with undo/redo for free.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
pub mod app;
pub mod diff_view;
pub mod doc;
pub mod highlight;
pub mod merge_view;
pub mod model;
pub mod resource;
pub mod samples;
mod web_file;

pub use actions::{DIFFMERGE_ACTION_TYPE, DiffmergeAction, HunkChoice};
pub use app::{DiffMergeApp, Mode};
pub use model::{DiffModel, MergeModel, Resolution};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(DiffMergeApp::new_demo())
}

/// Screenshot-only: the seeded demo with the transient floating status bubble
/// pinned open, so a single headless still shows the summary overlay (not a
/// bottom strip).
pub fn app_root_status() -> Box<dyn Widget> {
    Box::new(DiffMergeApp::for_shot(Some("status")))
}

/// Screenshot-only fixture states for visual regression review.
pub fn app_root_shot(scenario: Option<&str>) -> Box<dyn Widget> {
    Box::new(DiffMergeApp::for_shot(scenario))
}

pub fn app_root() -> Box<dyn Widget> {
    Box::new(DiffMergeApp::new())
}

/// The app as every host mounts it: the live root and its actions.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.diffmerge", app_root)
        .history(&framework_history::HISTORY)
        .component_actions(actions::register)
        .as_definition("Diff & Merge")
}

#[cfg(test)]
mod framework_history_tests;

pub mod framework_history;
#[cfg(target_arch = "wasm32")]
pub use resource::{Diffmerge, DiffmergeShell};
#[cfg(not(target_arch = "wasm32"))]
pub use resource::{Diffmerge, DiffmergeShell, launch_spec};
