//! Code — qOS native code editor / lightweight IDE.
//!
//! A file-tree sidebar, editor tabs, a syntax-highlighted code pane with line
//! numbers and a gutter, a command palette, a status bar, and a bottom panel
//! for problems/terminal. Buffer text lives in the shared Automerge document,
//! so edits are collaborative and undoable; colours come entirely from the OS
//! theme.
//!
//! `app_root()` is the single integration point used by both the live event
//! loop and the `--shots` offscreen renderer.

pub mod actions;
mod app;
mod bottom_panel;
mod command_palette;
mod doc;
mod editor_view;
mod file_tree;
mod highlight;
mod lint;
mod lsp;
mod model;
mod sample_project;
mod status_bar;
mod tab_bar;

use ui::widget::Widget;

pub use app::CodeApp;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(app::CodeApp::new())
}
