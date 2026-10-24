//! The overview column: the items of the chosen place in a [`VirtualList`],
//! one [`ListRow`] each, or the empty state when there are none.

use std::rc::Rc;

use accounts::SecretItem;
use taffy::{NodeId, TaffyTree};
use ui::icons::Icon;
use ui::widget::{AppModel, EmptyState, ListRow, ModelPath, VecListModel, VirtualList, Widget};
use vello::kurbo::Point;

use crate::vault::{Core, Request, Requests, Screen, Shared, VaultFilter, is_favorite, summary};

/// What a row shows of an item, keyed by the item's id.
struct Row {
    id: String,
    title: String,
    summary: String,
    favorite: bool,
}

impl Row {
    fn of(item: &SecretItem) -> Self {
        Self {
            id: item.id.0.clone(),
            title: item.title.clone(),
            summary: summary(item),
            favorite: is_favorite(item),
        }
    }
}

type Rows = VecListModel<Row, String>;

enum Body {
    List(Box<VirtualList<Rows>>),
    Empty(Box<EmptyState>),
}

/// The overview column widget, kept in step with the shared core.
pub(crate) struct ListPane {
    core: Shared,
    requests: Requests,
    body: Body,
    /// The list revision and place the body was built from.
    built: Option<(u64, VaultFilter)>,
    /// The open item to scroll into view once the list has its height.
    reveal: Option<String>,
    structure_dirty: bool,
}

impl ListPane {
    pub(crate) fn new(core: &Shared, requests: &Requests) -> Self {
        let mut pane = Self {
            core: Rc::clone(core),
            requests: Rc::clone(requests),
            body: Body::Empty(Box::new(EmptyState::new(""))),
            built: None,
            reveal: None,
            structure_dirty: false,
        };
        pane.refresh();
        pane
    }

    fn list(&self, rows: Rows, filter: VaultFilter) -> VirtualList<Rows> {
        let requests = Rc::clone(&self.requests);
        let activate = Rc::clone(&self.requests);
        VirtualList::with_selection_delegate(rows, 64.0, move |rows, index, id, _| {
            let Some(row) = rows.item(index) else {
                return Box::new(ListRow::new(format!("passwords.item:{id}"), ""));
            };
            let (open, toggle) = (Rc::clone(&requests), Rc::clone(&requests));
            let (open_id, toggle_id) = (secret(id), secret(id));
            let mut list_row = ListRow::new(format!("passwords.item:{id}"), &row.title)
                .avatar(&row.title, id)
                .square()
                .secondary(&row.summary)
                // A plain callback, not a published control: the vault window
                // keeps item titles out of what it publishes.
                .on_activate(move || open.borrow_mut().push(Request::Open(open_id.clone())))
                .action(
                    Icon::Star,
                    if row.favorite {
                        "Remove from favorites"
                    } else {
                        "Add to favorites"
                    },
                    move || {
                        toggle
                            .borrow_mut()
                            .push(Request::ToggleFavorite(toggle_id.clone()));
                    },
                );
            if row.favorite {
                list_row = list_row.mark(Icon::Star);
            }
            Box::new(list_row)
        })
        .filling()
        .with_id("passwords.items")
        .with_accessible_name(filter.label())
        .with_semantic_key(ToString::to_string)
        .on_activate(move |id| activate.borrow_mut().push(Request::Open(secret(&id))))
    }

    fn empty(&self, core: &Core) -> EmptyState {
        if core.opening {
            return EmptyState::new("Opening your vault")
                .with_icon(Icon::Shield)
                .busy();
        }
        let vault_empty = core.items.is_empty();
        let title = if vault_empty {
            "Your vault is empty"
        } else if core.filter == VaultFilter::Favorites {
            "No favorites yet"
        } else {
            "Nothing of this kind yet"
        };
        let empty = EmptyState::new(title).with_icon(if core.filter == VaultFilter::Favorites {
            Icon::Star
        } else {
            Icon::Shield
        });
        if !vault_empty {
            return empty;
        }
        vault_empty_state(&self.requests)
    }

    /// Rebuild what the core changed: the rows of a new revision, a fresh
    /// list for a new place, the open item and whether to scroll to it.
    fn refresh(&mut self) {
        let core = Rc::clone(&self.core);
        let mut core = core.borrow_mut();
        let reveal = std::mem::take(&mut core.reveal_open);
        let revision = (core.list_revision, core.filter);
        if self.built == Some(revision) && !reveal {
            return;
        }
        let same_place = self.built.is_some_and(|(_, filter)| filter == core.filter);
        self.built = Some(revision);
        let rows: Vec<Row> = core.visible().into_iter().map(Row::of).collect();
        if rows.is_empty() {
            self.body = Body::Empty(Box::new(self.empty(&core)));
            self.structure_dirty = true;
            return;
        }
        let Ok(rows) = VecListModel::new(rows, |row| row.id.clone()) else {
            return;
        };
        if same_place && let Body::List(list) = &mut self.body {
            list.update_model(|model| *model = rows);
        } else {
            self.body = Body::List(Box::new(self.list(rows, core.filter)));
            self.structure_dirty = true;
        }
        // A new item is not in the list yet: nothing there is the open item.
        let open = if core.screen == Screen::Editor && core.editing.is_none() {
            None
        } else {
            core.selected.as_ref().map(|id| id.0.clone())
        };
        if let Body::List(list) = &mut self.body {
            let selection = list.selection_mut();
            selection.clear();
            if let Some(open) = &open {
                selection.select_only(open.clone());
            }
        }
        if reveal || !same_place {
            self.reveal = open;
        }
    }
}

fn secret(id: &str) -> accounts::SecretId {
    accounts::SecretId(id.to_owned())
}

impl ui::widget::Decorator for ListPane {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        match &self.body {
            Body::List(list) => list.as_ref(),
            Body::Empty(empty) => empty.as_ref(),
        }
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        match &mut self.body {
            Body::List(list) => list.as_mut(),
            Body::Empty(empty) => empty.as_mut(),
        }
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.refresh();
        self.inner_mut().sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        self.refresh();
        self.inner_mut().update(state, path) || self.structure_dirty
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        self.inner_mut().take_structure_dirty() | own
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.inner_mut().apply_layout(taffy, node, origin);
        if let Body::List(list) = &mut self.body
            && let Some(open) = self.reveal.take()
        {
            list.reveal(&open);
        }
    }

    fn name(&self) -> &'static str {
        "PasswordsList"
    }
}

/// What an empty vault shows, across the whole window
/// ([`ui::widget::NavShell::nothing_listed`]) or in the list: what the vault
/// keeps, and New item.
pub(crate) fn vault_empty_state(requests: &Requests) -> EmptyState {
    let requests = Rc::clone(requests);
    EmptyState::new("Your vault is empty")
        .with_icon(Icon::Shield)
        .with_message(
            "Logins, secure notes, Wi-Fi passwords and API keys you save are encrypted and \
             kept here.",
        )
        .with_action("New item")
        .with_action_id("passwords.empty.new")
        .on_own_device_effect(move || requests.borrow_mut().push(Request::NewItem))
}
