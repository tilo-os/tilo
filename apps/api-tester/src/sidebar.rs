//! The navigation column: saved requests by collection, and history.
//!
//! Every request is a [`ListRow`] with its method as the leading code, so a
//! person scans for the verb first; history rows carry the status in their
//! meta and a status dot. The column is composed from the toolkit's content
//! widgets and holds no colour, size or padding of its own.

use ui::icons::Icon;
use ui::tokens::{Spacing, TextRole};
use ui::widget::{Button, Column, Element, ListRow, Scrollable, Section, SectionHeader, Text};

use crate::commands::{Cmd, FormMode, FormTarget, Sink};
use crate::model::{Collection, HistoryEntry, StatusClass};
use crate::palette::{short_path, status_class};

/// What the column shows.
pub(crate) struct SidebarView<'a> {
    pub(crate) collections: &'a [Collection],
    pub(crate) history: &'a [HistoryEntry],
    pub(crate) selected: Option<(usize, usize)>,
}

fn pusher(sink: &Sink, cmd: Cmd) -> impl FnMut() + 'static {
    let sink = sink.clone();
    move || sink.borrow_mut().push(cmd.clone())
}

fn quiet(text: &str) -> Element {
    Box::new(Text::new(text).with_role(TextRole::Caption).dimmed())
}

/// The environment picker over the lists, which scroll.
pub(crate) fn frame(environment: Element, lists: Element) -> Element {
    // The rows carry their own inset, so the column's gutter is the narrow
    // one the sidebar region uses.
    let gutter = Spacing::BASE.s3;
    Box::new(
        Column::new()
            .with_horizontal_padding(gutter, gutter)
            .with_vertical_padding(0.0)
            .push(environment)
            .push(Box::new(
                Column::new()
                    .with_no_padding()
                    .with_fill_remaining_height()
                    .push(lists),
            )),
    )
}

/// The collections and the history, scrolling as one.
pub(crate) fn lists(view: &SidebarView<'_>, sink: &Sink) -> Element {
    let column = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .push(Box::new(collections(view, sink)))
        .push(Box::new(history(view, sink)));
    Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(column)),
    )
}

fn collections(view: &SidebarView<'_>, sink: &Sink) -> Section {
    let new_collection = Button::icon_only("api.new-collection", Icon::Plus, "New collection")
        .on_effect(pusher(
            sink,
            Cmd::OpenForm(FormMode::NewCollection, FormTarget::default()),
        ));
    let mut items: Vec<Element> = Vec::new();
    if view.collections.is_empty() {
        items.push(quiet("No collections yet."));
    }
    for (ci, collection) in view.collections.iter().enumerate() {
        items.push(Box::new(collection_section(view, ci, collection, sink)));
    }
    Section::new(items).with_header(SectionHeader::heading("Collections").action(new_collection))
}

fn collection_section(
    view: &SidebarView<'_>,
    ci: usize,
    collection: &Collection,
    sink: &Sink,
) -> Section {
    let rename = Button::icon_only(
        &format!("api.rename-collection.{ci}"),
        Icon::Pencil,
        &format!("Rename collection {}", collection.name),
    )
    .on_effect(pusher(
        sink,
        Cmd::OpenForm(
            FormMode::RenameCollection,
            FormTarget {
                collection: Some(collection.id),
                request: None,
            },
        ),
    ));
    let mut rows: Vec<Element> = Vec::new();
    if collection.requests.is_empty() {
        rows.push(quiet("No saved requests"));
    }
    for (ri, request) in collection.requests.iter().enumerate() {
        let target = FormTarget {
            collection: Some(collection.id),
            request: Some(request.id),
        };
        let row = ListRow::new(format!("api.nav.request.{ci}.{ri}"), &request.name)
            .badge(request.method.as_str())
            .selected(view.selected == Some((ci, ri)))
            .on_activate(pusher(sink, Cmd::LoadRequest(ci, ri)))
            .action(
                Icon::Pencil,
                "Rename request",
                pusher(sink, Cmd::OpenForm(FormMode::RenameRequest, target)),
            )
            .action(
                Icon::Trash,
                "Delete request",
                pusher(sink, Cmd::OpenForm(FormMode::DeleteRequest, target)),
            );
        rows.push(Box::new(row));
    }
    Section::new(rows).with_header(
        SectionHeader::group(&collection.name)
            .count(collection.requests.len())
            .action(rename),
    )
}

fn history(view: &SidebarView<'_>, sink: &Sink) -> Section {
    let mut rows: Vec<Element> = Vec::new();
    if view.history.is_empty() {
        rows.push(quiet("No requests sent yet."));
    }
    for (hi, entry) in view.history.iter().enumerate() {
        let row = ListRow::new(format!("api.nav.history.{hi}"), short_path(&entry.url))
            .badge(entry.method.as_str())
            .meta(entry.status.to_string())
            .on_activate(pusher(sink, Cmd::ReplayHistory(hi)));
        let row = match status_class(entry.status) {
            StatusClass::Success | StatusClass::Redirect => row.active(),
            StatusClass::ClientError => row.warning(),
            StatusClass::ServerError => row.error(),
            StatusClass::Other => row.idle(),
        };
        rows.push(Box::new(row));
    }
    Section::new(rows).with_header(SectionHeader::heading("History"))
}
