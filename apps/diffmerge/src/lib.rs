//! Diff — a CRDT-native diff & merge tool for qOS, built on the `ui` toolkit.
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
pub mod portal;
pub mod samples;

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
    let mut app = DiffMergeApp::new_demo();
    app.arm_status_bubble_for_shots();
    Box::new(app)
}

pub fn app_root() -> Box<dyn Widget> {
    Box::new(DiffMergeApp::new())
}
