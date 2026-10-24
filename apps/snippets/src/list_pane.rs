//! The overview column: the snippets of the chosen place as a
//! [`VirtualList`] of [`ListRow`]s, or the empty state when there are none.
//!
//! The list is one focus stop: the arrow keys, Page Up and Down, Home and
//! End move the open snippet, and Enter brings it to the front of a narrow
//! window, as activating a row does.

use std::rc::Rc;

use taffy::{NodeId, TaffyTree};
use ui::icons::Icon;
use ui::widget::{
    AppModel, Element, EmptyState, ListRow, ModelPath, VecListModel, VirtualList, Widget,
};
use vello::kurbo::Point;

use crate::app::{Action, Requests, Shared};
use crate::model::{Selection, SmartFolder, Snippet, SnippetId, Snippets};

/// One row's snippet, with the badges of every row so they share a column.
struct Row {
    id: SnippetId,
    snippet: Snippet,
    badges: Rc<[String]>,
}

type Rows = VecListModel<Row, SnippetId>;

/// The secondary text of a row: what the snippet is for, or its first line
/// of code when it has no description.
fn secondary(snippet: &Snippet) -> &str {
    if snippet.description.trim().is_empty() {
        snippet.body.lines().next().unwrap_or("").trim()
    } else {
        snippet.description.as_str()
    }
}

/// The row of one snippet: its language badge (named by its tooltip), the
/// favorite star, what it is for and its variables; resting on it tells how
/// often it was used. Activating it opens the snippet.
fn row(row: &Row, requests: &Requests) -> Element {
    let snippet = &row.snippet;
    let open = Action::OpenSnippet(row.id.clone());
    let mut list_row = ListRow::new(open.semantic_id(), &snippet.title)
        .badge(snippet.language.badge())
        .badge_column(Rc::clone(&row.badges))
        .leading_tooltip(snippet.language.name())
        .secondary(secondary(snippet))
        .two_line_secondary()
        .tooltip(crate::tr_count("snippets-uses", snippet.use_count()));
    if snippet.favorite {
        list_row = list_row.mark(Icon::Star);
    }
    let count = snippet.placeholders().len();
    if count > 0 {
        list_row = list_row.meta(crate::tr_count("snippets-variables", count));
    }
    let requests = Rc::clone(requests);
    Box::new(list_row.on_view(move || requests.borrow_mut().push(open.clone())))
}

enum Body {
    List(Box<VirtualList<Rows>>),
    Empty(Box<EmptyState>),
}

/// The overview column widget, kept in step with the shared core.
pub(crate) struct ListPane {
    core: Shared,
    requests: Requests,
    body: Body,
    /// The library and place the body was built from, and the open snippet
    /// it shows.
    library: Rc<Snippets>,
    place: Option<Selection>,
    open: Option<SnippetId>,
    /// The open snippet to scroll into view once the list has its height.
    reveal: Option<SnippetId>,
    structure_dirty: bool,
}

impl ListPane {
    pub(crate) fn new(core: &Shared, requests: &Requests) -> Self {
        let mut pane = Self {
            core: Rc::clone(core),
            requests: Rc::clone(requests),
            body: Body::Empty(Box::new(EmptyState::new(""))),
            library: Rc::default(),
            place: None,
            open: None,
            reveal: None,
            structure_dirty: false,
        };
        pane.refresh();
        pane
    }

    fn list(&self, rows: Rows, name: String) -> VirtualList<Rows> {
        let requests = Rc::clone(&self.requests);
        let follow = Rc::clone(&self.core);
        let activate = Rc::clone(&self.requests);
        VirtualList::with_selection_delegate(rows, 72.0, move |rows, index, id, _| {
            rows.item(index).map_or_else(
                || {
                    Box::new(ListRow::new(
                        Action::OpenSnippet(id.clone()).semantic_id(),
                        "",
                    )) as Element
                },
                |item| row(item, &requests),
            )
        })
        .filling()
        .selection_follows_focus()
        // Moving through the list opens the snippet without bringing it to
        // the front of a narrow window.
        .on_select(move |id, selected| {
            if selected && let Ok(mut core) = follow.try_borrow_mut() {
                core.open = Some(id);
            }
        })
        .on_activate(move |id| activate.borrow_mut().push(Action::OpenSnippet(id)))
        .with_id("snippets.list")
        .with_accessible_name(name)
        .with_semantic_key(ToString::to_string)
    }

    /// The zero state of a place without snippets: one sentence saying what
    /// would fill it. Creating a snippet is the header's action.
    fn empty(selection: &Selection) -> EmptyState {
        let (title, icon) = match selection {
            Selection::Smart(SmartFolder::Favorites) => ("snippets-empty-favorites", Icon::Star),
            Selection::Smart(SmartFolder::MostUsed) => ("snippets-empty-most-used", Icon::History),
            _ => ("snippets-empty-place", Icon::Inbox),
        };
        EmptyState::new(crate::tr(title)).with_icon(icon)
    }

    /// Follow the core: new rows for a new library, a fresh list for a new
    /// place, the open snippet as the selected row, and whether to scroll
    /// to it.
    fn refresh(&mut self) {
        let core = Rc::clone(&self.core);
        let Ok(mut core) = core.try_borrow_mut() else {
            return;
        };
        let reveal = core.reveal.take();
        let same_place = self.place.as_ref() == Some(&core.selection);
        let same_rows = same_place && Rc::ptr_eq(&self.library, &core.library);
        if !same_rows {
            self.library = Rc::clone(&core.library);
            self.place = Some(core.selection.clone());
            let snippets: Vec<(SnippetId, Snippet)> = core
                .visible
                .iter()
                .filter_map(|id| core.library.get(id).map(|s| (id.clone(), s.clone())))
                .collect();
            if snippets.is_empty() {
                self.body = Body::Empty(Box::new(Self::empty(&core.selection)));
                self.structure_dirty = true;
            } else {
                let mut badges: Vec<String> = snippets
                    .iter()
                    .map(|(_, s)| s.language.badge().to_owned())
                    .collect();
                badges.sort_unstable();
                badges.dedup();
                let badges: Rc<[String]> = badges.into();
                let rows = snippets
                    .into_iter()
                    .map(|(id, snippet)| Row {
                        id,
                        snippet,
                        badges: Rc::clone(&badges),
                    })
                    .collect();
                let Ok(rows) = VecListModel::new(rows, |row: &Row| row.id.clone()) else {
                    return;
                };
                if same_place && let Body::List(list) = &mut self.body {
                    list.update_model(|model| *model = rows);
                } else {
                    let list = self.list(rows, core.selection_title());
                    self.body = Body::List(Box::new(list));
                    self.structure_dirty = true;
                }
            }
        }
        let open = core.open.clone();
        if let Body::List(list) = &mut self.body
            && (!same_rows || open != self.open)
        {
            let selection = list.selection_mut();
            selection.clear();
            if let Some(open) = &open {
                selection.select_only(open.clone());
            }
        }
        if !same_place || open != self.open {
            self.reveal.clone_from(&open);
        }
        if reveal.is_some() {
            self.reveal = reveal;
        }
        self.open = open;
    }
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
        "SnippetList"
    }
}
