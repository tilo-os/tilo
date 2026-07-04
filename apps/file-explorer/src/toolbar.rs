//! Toolbar widget-tree builder. Emits a `Row` of `quick-ui` buttons (with
//! crisp vector icons) plus the search field, each wired to the app's
//! [`CommandSink`](crate::command::CommandSink). Pure function — owns no
//! state.

use crate::command::{push, CommandSink, FileCommand};
use crate::model::{SortOrder, ViewMode};
use ui::icons::Icon;
use ui::widget::{Button, Row, Spacer, TextInput, Widget};

pub struct ToolbarSnapshot<'a> {
    pub search_query: &'a str,
    pub view_mode: ViewMode,
    pub sort_order: SortOrder,
    pub can_back: bool,
    pub can_forward: bool,
    pub can_up: bool,
}

/// Compact toolbar icon-button size.
const TB_BTN_W: f32 = 40.0;
const TB_BTN_H: f32 = 36.0;

/// Build a compact icon button wired to push `cmd` when clicked.
fn icon_button(id: &str, icon: Icon, sink: &CommandSink, cmd: FileCommand) -> Box<dyn Widget> {
    let sink = sink.clone();
    Box::new(
        Button::new(id, "", None)
            .with_icon(icon)
            .with_size(TB_BTN_W, TB_BTN_H)
            .on_click(move || push(&sink, cmd.clone())),
    )
}

/// Emit the toolbar widget tree.
pub fn build_toolbar(snapshot: ToolbarSnapshot<'_>, sink: &CommandSink) -> Box<dyn Widget> {
    let _ = snapshot.search_query; // shown by the search field via the CRDT
    let _ = snapshot.can_back;
    let _ = snapshot.can_forward;
    let _ = snapshot.can_up;
    let _ = snapshot.sort_order;

    Box::new(
        Row::new()
            .no_wrap()
            .push(icon_button("files.back", Icon::ArrowLeft, sink, FileCommand::Back))
            .push(icon_button("files.forward", Icon::ArrowRight, sink, FileCommand::Forward))
            .push(icon_button("files.up", Icon::ArrowUp, sink, FileCommand::Up))
            .push(icon_button("files.refresh", Icon::RotateCw, sink, FileCommand::Refresh))
            .push(Box::new(
                TextInput::new(vec![])
                    .with_id("files.search")
                    .with_on_change_path("files.search_query"),
            ))
            .push(Box::new(Spacer::new()))
            .push(icon_button("files.newfolder", Icon::FolderPlus, sink, FileCommand::NewFolder))
            .push(icon_button("files.view.list", Icon::List, sink, FileCommand::SetView(ViewMode::List)))
            .push(icon_button("files.view.grid", Icon::LayoutGrid, sink, FileCommand::SetView(ViewMode::Grid)))
            .push(icon_button("files.sort.toggle", Icon::ArrowUpDown, sink, FileCommand::ToggleSortOrder))
            .push(icon_button("files.open_terminal", Icon::Code, sink, FileCommand::OpenTerminal)),
    )
}
