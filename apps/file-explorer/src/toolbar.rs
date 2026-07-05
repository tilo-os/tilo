//! Toolbar widget-tree builder. Emits a `Row` of `quick-ui` buttons (with
//! crisp vector icons) plus the search field, each wired to the app's
//! [`CommandSink`](crate::command::CommandSink). Pure function — owns no
//! state.

use crate::command::{push, CommandSink, FileCommand};
use crate::model::{SortOrder, ViewMode};
use ui::icons::Icon;
use ui::widget::toggle_group::Segment;
use ui::widget::{Button, Row, Spacer, TextInput, ToggleGroup, Widget};

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

/// Build a compact icon button wired to push `cmd` when clicked. `enabled`
/// drives the dimmed/inert affordance so nav buttons visibly reflect whether
/// there's anywhere to go.
fn icon_button(id: &str, icon: Icon, sink: &CommandSink, cmd: FileCommand, enabled: bool) -> Box<dyn Widget> {
    let sink = sink.clone();
    Box::new(
        Button::new(id, "", None)
            .with_icon(icon)
            .with_size(TB_BTN_W, TB_BTN_H)
            .with_enabled(enabled)
            .on_click(move || push(&sink, cmd.clone())),
    )
}

/// Emit the toolbar widget tree.
pub fn build_toolbar(snapshot: ToolbarSnapshot<'_>, sink: &CommandSink) -> Box<dyn Widget> {
    let _ = snapshot.search_query; // shown by the search field via the CRDT

    // View-mode switch as a real segmented control so the active view is
    // visibly selected (a plain pair of buttons couldn't show which is live).
    let view_sink = sink.clone();
    let selected_view = match snapshot.view_mode {
        ViewMode::List => 0,
        ViewMode::Grid => 1,
    };
    let view_switch = ToggleGroup::new(
        vec![Segment::new("List"), Segment::new("Grid")],
        selected_view,
    )
    .on_change(move |i| {
        let mode = if i == 0 { ViewMode::List } else { ViewMode::Grid };
        push(&view_sink, FileCommand::SetView(mode));
    });

    // Sort-order toggle: the glyph tracks the live direction so the button
    // isn't a state-less no-op (chevron up = ascending, down = descending).
    let sort_icon = match snapshot.sort_order {
        SortOrder::Ascending => Icon::ChevronUp,
        SortOrder::Descending => Icon::ChevronDown,
    };

    Box::new(
        Row::new()
            .no_wrap()
            .push(icon_button("files.back", Icon::ArrowLeft, sink, FileCommand::Back, snapshot.can_back))
            .push(icon_button("files.forward", Icon::ArrowRight, sink, FileCommand::Forward, snapshot.can_forward))
            .push(icon_button("files.up", Icon::ArrowUp, sink, FileCommand::Up, snapshot.can_up))
            .push(icon_button("files.refresh", Icon::RotateCw, sink, FileCommand::Refresh, true))
            .push(Box::new(
                TextInput::new(vec![])
                    .with_id("files.search")
                    .with_on_change_path("files.search_query")
                    .with_placeholder("Search"),
            ))
            .push(Box::new(Spacer::new()))
            .push(icon_button("files.newfolder", Icon::FolderPlus, sink, FileCommand::NewFolder, true))
            .push(Box::new(view_switch))
            .push(icon_button("files.sort.toggle", sort_icon, sink, FileCommand::ToggleSortOrder, true))
            .push(icon_button("files.open_terminal", Icon::Code, sink, FileCommand::OpenTerminal, true)),
    )
}
