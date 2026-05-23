//! Toolbar widget-tree builder. Uses `quick-ui` widgets (Button, TextInput,
//! Row, Spacer). Pure function — owns no state; reads from
//! `FileExplorerWidget` and wires events back into it via the CRDT.
//!
//! The concrete widget-tree is structural for Task 10: it emits a Row that
//! nests the expected affordances (back, refresh, search, view mode, sort).
//! Full visual styling / behaviour parity lands in later tasks (T11–T13).

use crate::model::{SortBy, SortOrder, ViewMode};
use ui::widget::{Button, Row, Spacer, TextInput, Widget};

pub struct ToolbarSnapshot<'a> {
    pub search_query: &'a str,
    pub view_mode: ViewMode,
    pub sort_by: SortBy,
    pub sort_order: SortOrder,
}

/// Emits a toolbar widget tree. The callback parameters are kept so later
/// tasks can wire them into the concrete widgets; for Task 10 we emit a
/// minimal Row with the expected affordances so draw/layout is a valid
/// tree and the API shape is locked in.
#[allow(clippy::too_many_arguments)]
pub fn build_toolbar(
    snapshot: ToolbarSnapshot<'_>,
    _on_back: Box<dyn FnMut()>,
    _on_refresh: Box<dyn FnMut()>,
    _on_search_change: Box<dyn FnMut(String)>,
    _on_view_mode: Box<dyn FnMut(ViewMode)>,
    _on_sort_by: Box<dyn FnMut(SortBy)>,
    _on_toggle_order: Box<dyn FnMut()>,
    _on_open_terminal: Box<dyn FnMut()>,
) -> Box<dyn Widget> {
    let _ = snapshot.search_query;
    let _ = snapshot.view_mode;
    let _ = snapshot.sort_by;
    let _ = snapshot.sort_order;

    Box::new(
        Row::new()
            .push(Box::new(Button::new("files.back", "󰁍", None)))
            .push(Box::new(Button::new("files.refresh", "󰑐", None)))
            .push(Box::new(
                TextInput::new(vec![])
                    .with_id("files.search")
                    .with_on_change_path("files.search_query"),
            ))
            .push(Box::new(Spacer::new()))
            .push(Box::new(Button::new("files.view.list", "󰉩", None)))
            .push(Box::new(Button::new("files.view.grid", "󰕰", None)))
            .push(Box::new(Button::new("files.sort.toggle", "󰒺", None)))
            .push(Box::new(Button::new("files.open_terminal", "󰆍", None))),
    )
}
