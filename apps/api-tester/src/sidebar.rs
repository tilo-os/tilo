//! The navigation column: saved requests by collection, and history.
//!
//! The requests are the toolkit's [`NavList`]: a section per collection,
//! each request led by its method as a badge, so the names of a collection
//! share one edge whatever the verb; history rows are led by their method
//! too and carry the status as their meta and a status dot. The column holds
//! no colour, size or padding of its own.
//!
//! The list lives as long as the window and is handed new sections as the
//! workspace changes, so the person's place in it (keyboard focus, scroll)
//! survives choosing a request.

use std::cell::RefCell;
use std::rc::Rc;

use ui::icons::Icon;
use ui::tokens::Spacing;
use ui::widget::{
    AppModel, Button, Column, Element, ModelPath, NavAction, NavItem, NavList, NavSection,
    RowStatus, Widget,
};

use crate::commands::{Cmd, FormMode, FormTarget, Sink};
use crate::model::{Collection, HistoryEntry, Id, StatusClass};
use crate::palette::{short_path, status_class};

/// What the column shows.
pub(crate) struct SidebarView<'a> {
    pub(crate) collections: &'a [Collection],
    pub(crate) history: &'a [HistoryEntry],
    pub(crate) selected: Option<(usize, usize)>,
}

const RENAME: &str = "rename";
const DELETE: &str = "delete";

/// The "New collection" action in the sidebar's header.
pub(crate) fn new_collection(sink: &Sink) -> Element {
    let sink = sink.clone();
    Box::new(
        Button::icon_only("api.new-collection", Icon::Plus, "New collection").on_effect(
            move || {
                sink.borrow_mut().push(Cmd::OpenForm(
                    FormMode::NewCollection,
                    FormTarget::default(),
                ));
            },
        ),
    )
}

/// The environment picker over the lists.
pub(crate) fn frame(environment: Element, lists: Element) -> Element {
    // The list insets its own rows; the picker takes the sidebar's gutter.
    let gutter = Spacing::BASE.s3;
    Box::new(
        Column::new()
            .with_no_padding()
            .push(Box::new(
                Column::new()
                    .with_intrinsic_height()
                    .with_horizontal_padding(gutter, gutter)
                    .with_vertical_padding(0.0)
                    .push(environment),
            ))
            // The list takes the rest of the column and scrolls itself.
            .push(lists),
    )
}

fn request_id(ci: usize, ri: usize) -> String {
    format!("request:{ci}:{ri}")
}

fn history_id(hi: usize) -> String {
    format!("history:{hi}")
}

fn collection_section_id(ci: usize) -> String {
    format!("collection:{ci}")
}

/// Parse `request:{ci}:{ri}`.
fn parse_request(id: &str) -> Option<(usize, usize)> {
    let (ci, ri) = id.strip_prefix("request:")?.split_once(':')?;
    Some((ci.parse().ok()?, ri.parse().ok()?))
}

/// The sections the list shows.
fn sections(view: &SidebarView<'_>) -> Vec<NavSection> {
    let mut sections: Vec<NavSection> = view
        .collections
        .iter()
        .enumerate()
        .map(|(ci, collection)| {
            let items = collection
                .requests
                .iter()
                .enumerate()
                .map(|(ri, request)| {
                    NavItem::new(request_id(ci, ri), &request.name).badge(request.method.as_str())
                })
                .collect();
            NavSection::new(collection_section_id(ci), &collection.name, items)
                .collapsible()
                .trailing_action(NavAction::new(RENAME, Icon::Pencil, "Rename collection"))
                .empty_caption("No saved requests")
        })
        .collect();
    if sections.is_empty() {
        sections.push(
            NavSection::new("collections", "Collections", Vec::new())
                .empty_caption("No collections yet"),
        );
    }
    let history = view
        .history
        .iter()
        .enumerate()
        .map(|(hi, entry)| {
            let status = match status_class(entry.status) {
                StatusClass::Success | StatusClass::Redirect => RowStatus::Active,
                StatusClass::ClientError => RowStatus::Warning,
                StatusClass::ServerError => RowStatus::Error,
                StatusClass::Other => RowStatus::Idle,
            };
            NavItem::new(history_id(hi), short_path(&entry.url))
                .badge(entry.method.as_str())
                .meta(entry.status.to_string())
                .status(status)
                .without_actions()
        })
        .collect();
    sections
        .push(NavSection::new("history", "History", history).empty_caption("No requests sent yet"));
    sections
}

/// The ids behind each collection and its requests, by position, so a row's
/// action reaches the right one after the list is handed new sections.
type Ids = Rc<RefCell<Vec<(Id, Vec<Id>)>>>;

fn ids_of(view: &SidebarView<'_>) -> Vec<(Id, Vec<Id>)> {
    view.collections
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                c.requests.iter().map(|r| r.id.clone()).collect(),
            )
        })
        .collect()
}

/// The sections and the chosen row the list takes at its next
/// synchronization.
type Pending = Rc<RefCell<Option<(Vec<NavSection>, Option<String>)>>>;

/// What the app holds to update the list.
#[derive(Clone)]
pub(crate) struct NavHandle {
    pending: Pending,
    ids: Ids,
}

impl NavHandle {
    /// Show `view` at the list's next synchronization.
    pub(crate) fn set(&self, view: &SidebarView<'_>) {
        *self.ids.borrow_mut() = ids_of(view);
        *self.pending.borrow_mut() = Some((
            sections(view),
            view.selected.map(|(ci, ri)| request_id(ci, ri)),
        ));
    }
}

/// The collections and the history in one navigation list.
pub(crate) struct RequestsNav {
    list: NavList,
    pending: Pending,
}

/// The list, and the handle the app updates it through.
pub(crate) fn lists(view: &SidebarView<'_>, sink: &Sink) -> (RequestsNav, NavHandle) {
    let ids: Ids = Rc::new(RefCell::new(ids_of(view)));
    let select_sink = sink.clone();
    let action_sink = sink.clone();
    let section_sink = sink.clone();
    let action_ids = Rc::clone(&ids);
    let section_ids = Rc::clone(&ids);
    let mut list = NavList::new(sections(view))
        .with_id("api.nav")
        .with_accessible_name("Requests")
        .on_select(move |id| {
            let cmd = if let Some((ci, ri)) = parse_request(&id) {
                Cmd::LoadRequest(ci, ri)
            } else if let Some(hi) = id.strip_prefix("history:").and_then(|hi| hi.parse().ok()) {
                Cmd::ReplayHistory(hi)
            } else {
                return;
            };
            select_sink.borrow_mut().push(cmd);
        })
        .item_actions(
            vec![
                NavAction::new(RENAME, Icon::Pencil, "Rename request"),
                NavAction::new(DELETE, Icon::Trash, "Delete request"),
            ],
            move |id, action| {
                let Some((ci, ri)) = parse_request(id) else {
                    return;
                };
                let ids = action_ids.borrow();
                let collection = ids.get(ci);
                let target = FormTarget {
                    collection: collection.map(|(id, _)| id.clone()),
                    request: collection
                        .and_then(|(_, requests)| requests.get(ri))
                        .cloned(),
                };
                let cmd = if action == DELETE {
                    Cmd::DeleteRequest(target)
                } else {
                    Cmd::OpenForm(FormMode::RenameRequest, target)
                };
                action_sink.borrow_mut().push(cmd);
            },
        )
        .on_section_action(move |section, _| {
            let Some(ci) = section
                .strip_prefix("collection:")
                .and_then(|ci| ci.parse::<usize>().ok())
            else {
                return;
            };
            section_sink.borrow_mut().push(Cmd::OpenForm(
                FormMode::RenameCollection,
                FormTarget {
                    collection: section_ids.borrow().get(ci).map(|(id, _)| id.clone()),
                    request: None,
                },
            ));
        });
    list.set_selected(view.selected.map(|(ci, ri)| request_id(ci, ri)).as_deref());
    let pending = Rc::new(RefCell::new(None));
    (
        RequestsNav {
            list,
            pending: Rc::clone(&pending),
        },
        NavHandle { pending, ids },
    )
}

impl RequestsNav {
    fn apply_pending(&mut self) {
        if let Some((sections, selected)) = self.pending.borrow_mut().take() {
            self.list.set_sections(sections);
            if self.list.selected_id() != selected {
                self.list.set_selected(selected.as_deref());
            }
        }
    }
}

impl ui::widget::Decorator for RequestsNav {
    type Inner = NavList;

    fn inner(&self) -> &NavList {
        &self.list
    }

    fn inner_mut(&mut self) -> &mut NavList {
        &mut self.list
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.apply_pending();
        Widget::sync(&mut self.list, state, path);
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        self.apply_pending();
        Widget::settle(&mut self.list, edits, path);
    }

    fn needs_idle_sync(&self) -> bool {
        self.pending.borrow().is_some() || Widget::needs_idle_sync(&self.list)
    }

    fn name(&self) -> &'static str {
        "RequestsNav"
    }
}
