//! The response pane: nothing yet, a failure, or the response with its status,
//! timing, size and body.

use ui::icons::Icon;
use ui::text_render::TextKind;
use ui::tokens::TextRole;
use ui::widget::data_table::{ColumnDef, ColumnWidth, DataTable};
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Badge, ChipStyle, Column, ControlSize, Element, EmptyState, Row, Scrollable, Spacer, Text,
    ToggleGroup, Tone,
};

use crate::commands::{Cmd, Sink};
use crate::model::{KeyValue, ResponseData, ResponseView, human_duration};
use crate::palette::status_tone;
use crate::swap::{Swap, SwapHandle};

/// A built response pane and the slot its body view can be replaced in.
pub(crate) struct Built {
    pub(crate) element: Element,
    pub(crate) body: SwapHandle,
}

/// No request has been sent yet.
pub(crate) fn empty() -> Element {
    Box::new(
        EmptyState::new("No response yet")
            .with_icon(Icon::Globe)
            .with_message("Send the request to see what comes back.")
            .into_scrollable(),
    )
}

/// A request is on its way.
pub(crate) fn sending() -> Element {
    Box::new(
        EmptyState::new("Sending request")
            .with_icon(Icon::Send)
            .with_message("Waiting for the server to respond.")
            .into_scrollable(),
    )
}

/// The request could not be completed. The reason is shown here once.
pub(crate) fn failed(detail: &str) -> Element {
    let status = Row::new().push(Box::new(
        Badge::text("Failed").style(ChipStyle::Tinted(Tone::Danger)),
    ));
    let reason = EmptyState::error("Couldn't complete the request", detail)
        .with_icon(Icon::TriangleAlert)
        .with_action("Try again")
        .with_action_id("api.response.retry")
        .on_action(crate::app::active_send_binding())
        .into_scrollable();
    Box::new(
        Column::new()
            .with_no_padding()
            .push(Box::new(status))
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_fill_remaining_height()
                    .push(Box::new(reason)),
            )),
    )
}

fn caption(text: &str) -> Element {
    Box::new(
        Text::new(text)
            .with_role(TextRole::Caption)
            .numeric()
            .dimmed(),
    )
}

/// The status, timing and size, and the switch between views of the body.
pub(crate) fn response(data: &ResponseData, view: ResponseView, sink: &Sink) -> Built {
    let view_sink = sink.clone();
    let switch = ToggleGroup::new(
        ResponseView::ALL
            .iter()
            .map(|v| Segment::new(v.label()))
            .collect(),
        ResponseView::ALL
            .iter()
            .position(|v| *v == view)
            .unwrap_or(0),
    )
    .control_size(ControlSize::Regular)
    .on_view("api.response-view", move |i| {
        view_sink.borrow_mut().push(Cmd::SetView(i));
    });
    let status = Badge::text(format!("{} {}", data.status, data.status_text))
        .style(ChipStyle::Tinted(status_tone(data.status_class())));
    let size = ui::format::bytes(u64::try_from(data.size_bytes).unwrap_or(u64::MAX));
    let summary = Row::new()
        .push(Box::new(status))
        .push(caption(&human_duration(data.duration_ms)))
        .push(caption(&size))
        .push(Box::new(Spacer::new()))
        .push(Box::new(switch));
    let (slot, handle) = Swap::new(body(data, view));
    let column = Column::new()
        .with_no_padding()
        .push(Box::new(summary))
        .push(Box::new(slot));
    Built {
        element: Box::new(column),
        body: handle,
    }
}

/// The body in the chosen view.
pub(crate) fn body(data: &ResponseData, view: ResponseView) -> Element {
    match view {
        ResponseView::Pretty if data.is_json() => Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(Box::new(crate::code_view::CodeView::json(
                    "Response body",
                    &data.body,
                ))),
        ),
        ResponseView::Pretty | ResponseView::Raw => Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(Box::new(crate::code_view::CodeView::plain(
                    "Response body",
                    &data.body,
                ))),
        ),
        ResponseView::Headers => headers(&data.headers),
    }
}

fn headers(rows: &[KeyValue]) -> Element {
    let columns = vec![
        ColumnDef::new("name", "Name", ColumnWidth::Flex(1.0))
            .with_kind(TextKind::Identifier)
            .with_text(|row: &KeyValue| row.key.clone()),
        ColumnDef::new("value", "Value", ColumnWidth::Flex(2.0))
            .with_kind(TextKind::Identifier)
            .with_text(|row: &KeyValue| row.value.clone()),
    ];
    let mut table = DataTable::new(columns).with_accessible_name("Response headers");
    table.set_rows(rows.to_vec());
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(table)),
    )
}
