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

use crate::actions::ApiTesterEffect;
use crate::commands::{Cmd, FormMode, FormTarget, Sink};
use crate::model::{Collection, HistoryEntry, Id, StatusClass};
use crate::palette::{short_path, status_class};

/// What the column shows.
pub(crate) struct SidebarView<'a> {
    pub(crate) collections: &'a [Collection],
    pub(crate) history: &'a [HistoryEntry],
    pub(crate) selected: Option<(usize, usize)>,
}

/// The collection header's action.
const RENAME: &str = "rename";

/// The "New collection" action in the sidebar's header.
pub(crate) fn new_collection(sink: &Sink) -> Element {
    let sink = sink.clone();
    Box::new(
        // It opens the form; the collection it creates is its own action.
        Button::icon_only("api.new-collection", Icon::Plus, "New collection").on_own_device_effect(
            move || {
                sink.borrow_mut().push(Cmd::OpenForm(
                    FormMode::NewCollection,
                    FormTarget::default(),
                ));
            },
        ),
    )
}

pub(crate) fn frame(environment: Element, lists: Element) -> Element {
    // The list insets its own rows; the picker takes the sidebar's gutter,
    // above it too, where the window bar ends.
    let gutter = Spacing::BASE.s3;
    Box::new(
        Column::new()
            .with_no_padding()
            .push(Box::new(
                Column::new()
                    .with_intrinsic_height()
                    .with_horizontal_padding(gutter, gutter)
                    .with_vertical_padding(gutter)
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

/// A collection's section, by the collection's id: what the header's
/// rename runs with.
fn collection_section_id(collection: &Id) -> String {
    format!("collection:{collection}")
}

/// Parse `request:{ci}:{ri}`.
pub(crate) fn parse_request(id: &str) -> Option<(usize, usize)> {
    let (ci, ri) = id.strip_prefix("request:")?.split_once(':')?;
    Some((ci.parse().ok()?, ri.parse().ok()?))
}

/// Parse `history:{hi}`.
pub(crate) fn parse_history(id: &str) -> Option<usize> {
    id.strip_prefix("history:")?.parse().ok()
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
            NavSection::new(
                collection_section_id(&collection.id),
                &collection.name,
                items,
            )
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
        })
        .collect();
    sections
        .push(NavSection::new("history", "History", history).empty_caption("No requests sent yet"));
    sections
}

/// The sections and the chosen row the list takes at its next
/// synchronization.
type Pending = Rc<RefCell<Option<(Vec<NavSection>, Option<String>)>>>;

/// What the app holds to update the list.
#[derive(Clone)]
pub(crate) struct NavHandle {
    pending: Pending,
}

impl NavHandle {
    /// Show `view` at the list's next synchronization.
    pub(crate) fn set(&self, view: &SidebarView<'_>) {
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
    let select_sink = sink.clone();
    let menu_sink = sink.clone();
    let mut list = NavList::new(sections(view))
        .with_accessible_name("Requests")
        .on_view("api.nav", move |id| {
            let cmd = if let Some((ci, ri)) = parse_request(&id) {
                Cmd::LoadRequest(ci, ri)
            } else if let Some(hi) = id.strip_prefix("history:").and_then(|hi| hi.parse().ok()) {
                Cmd::ReplayHistory(hi)
            } else {
                return;
            };
            select_sink.borrow_mut().push(cmd);
        })
        // One More on every row, opening that row's own actions: a saved
        // request is renamed or deleted, a sent one is replayed, kept as a
        // request, or forgotten (`APP-api-tester-04`). The app decides what a
        // row offers, so a history row is never offered a rename.
        .item_menu(NavAction::new("more", Icon::MoreHorizontal, "More"))
        .on_context_menu(move |id, lift, at| {
            menu_sink.borrow_mut().push(Cmd::RowMenu {
                id: id.to_owned(),
                lift,
                at,
            });
        });
    // A collection's header renames it, through the form its effect opens.
    if let Some(rename) = sink.actions.borrow().as_ref().and_then(|actions| {
        actions.effect_binding(
            ApiTesterEffect::RENAME_COLLECTION
                .try_map(|section: String| section.strip_prefix("collection:").map(str::to_owned)),
        )
    }) {
        list = list.section_action(RENAME, rename);
    }
    list.set_selected(view.selected.map(|(ci, ri)| request_id(ci, ri)).as_deref());
    let pending = Rc::new(RefCell::new(None));
    (
        RequestsNav {
            list,
            pending: Rc::clone(&pending),
        },
        NavHandle { pending },
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
