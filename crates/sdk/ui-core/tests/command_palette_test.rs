#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};
use tilo_ui::widget::command_palette::{CommandEntry, CommandPalette};

#[test]
fn command_palette_filters_by_query() {
    let mut p = CommandPalette::new();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let h = hits.clone();
    p.push(
        CommandEntry::new("ws.create", "Create Workspace")
            .on_select(move |_, _| h.lock().unwrap().push("create")),
    );
    let h = hits.clone();
    p.push(
        CommandEntry::new("ws.rename", "Rename Workspace")
            .on_select(move |_, _| h.lock().unwrap().push("rename")),
    );
    p.set_query("ren");
    assert_eq!(p.visible_entries().len(), 1);
    assert_eq!(p.visible_entries()[0].label, "Rename Workspace");
}

#[test]
fn command_palette_custom_filter_is_composed() {
    let mut p = CommandPalette::new();
    p.push(CommandEntry::new("file.open", "Open File"));
    p.push(CommandEntry::new("ws.create", "Create Workspace"));
    p.set_filter(Box::new(|e| e.id.starts_with("ws.")));
    p.set_query("create");
    assert_eq!(p.visible_entries().len(), 1);
    assert_eq!(p.visible_entries()[0].id, "ws.create");
}
