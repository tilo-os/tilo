#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

//! The editor as a person and a screen reader meet it: hosted by the app
//! framework, driven through keys, pointer and the accessibility tree, and
//! read back through the project the host holds. One binary, so the editor
//! links once for all of them.

mod a11y_tree;
mod bin_and_imports;
mod bin_and_titles;
mod export_dialog;
mod export_status;
mod hosted;
mod hosted_editor;
mod inspector_controls;
mod inspector_reader;
mod inspector_sliders;
mod keyboard_edits;
mod keyboard_navigation;
mod linked_shots;
mod marker_list;
mod markers;
mod panels;
mod project_files;
mod range_marks;
mod timeline_view;
mod transport_and_layout;
