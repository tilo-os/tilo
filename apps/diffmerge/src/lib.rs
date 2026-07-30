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

pub mod actions;
pub mod app;
pub mod diff_view;
pub mod doc;
pub mod highlight;
pub mod merge_view;
pub mod model;
pub mod portal;
pub mod samples;

pub use actions::{DiffmergeAction, HunkChoice, DIFFMERGE_ACTION_TYPE};
pub use app::{DiffMergeApp, Mode};
pub use model::{DiffModel, MergeModel, Resolution};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root_demo() -> Box<dyn Widget> {
    Box::new(DiffMergeApp::new_demo())
}

pub fn app_root() -> Box<dyn Widget> {
    Box::new(DiffMergeApp::new())
}
