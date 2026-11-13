//! What a navigation list shows: the sections and items an app hands over, flattened into the
//! rows the list scrolls, with the person's folds and the window's filter applied.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use vello::peniko::Color;

use super::{NavItem, NavSection};
use crate::chrome::NavCount;
use crate::icons::Icon;
use crate::widget::{ListModel, RowStatus, Tone};

/// An owned [`RowStatus`], as a navigation item keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NavStatus {
    Active,
    Warning,
    Error,
    Idle,
    Presence(String),
}

impl NavStatus {
    pub(crate) fn of(status: RowStatus<'_>) -> Self {
        match status {
            RowStatus::Active => Self::Active,
            RowStatus::Warning => Self::Warning,
            RowStatus::Error => Self::Error,
            RowStatus::Idle => Self::Idle,
            RowStatus::Presence(id) => Self::Presence(id.to_owned()),
        }
    }

    pub(crate) fn row_status(&self) -> RowStatus<'_> {
        match self {
            Self::Active => RowStatus::Active,
            Self::Warning => RowStatus::Warning,
            Self::Error => RowStatus::Error,
            Self::Idle => RowStatus::Idle,
            Self::Presence(id) => RowStatus::Presence(id),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NavItemSpec {
    pub icon: Option<Icon>,
    pub count: Option<NavCount>,
    pub depth: usize,
    pub under_section: bool,
    pub secondary: Option<String>,
    pub meta: Option<String>,
    pub status: Option<NavStatus>,
    pub status_label: Option<(String, Tone)>,
    pub badge: Option<String>,
    pub section_badges: Rc<Vec<String>>,
    pub swatch: Option<Color>,
    pub art: Option<super::NavArt>,
    pub checked: Option<bool>,
    pub level: Option<f32>,
    pub presence: Vec<String>,
    pub person: Option<super::NavPerson>,
    pub unread: bool,
    pub shows: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct NavRowData {
    pub(crate) key: String,
    pub(crate) kind: NavRowKind,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum NavRowKind {
    /// A section header. `first` is the first row of the list, which has no
    /// space above it.
    Section {
        id: String,
        title: String,
        collapsible: bool,
        open: bool,
        action: Option<Icon>,
        action_label: Option<String>,
        first: bool,
        editable: bool,
        menu: bool,
    },
    /// A destination.
    Item {
        id: String,
        label: String,
        spec: Box<NavItemSpec>,
        branch: bool,
        open: bool,
        actions: bool,
        drop_target: bool,
        editable: bool,
        semantic_key: Option<String>,
    },
    /// The one quiet line an empty section shows ("No drafts").
    Caption { id: String, caption: String },
    /// A row standing in for an item still being read.
    Pending { id: String },
}

impl NavRowKind {
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Section { id, .. }
            | Self::Caption { id, .. }
            | Self::Pending { id }
            | Self::Item { id, .. } => id,
        }
    }

    /// What assistive technology and tests name the row by.
    pub(crate) fn semantic_key(&self) -> Option<String> {
        match self {
            Self::Section { id, .. } => Some(format!("nav:section:{id}")),
            Self::Item {
                semantic_key, id, ..
            } => Some(
                semantic_key
                    .clone()
                    .unwrap_or_else(|| format!("nav:item:{id}")),
            ),
            Self::Caption { .. } | Self::Pending { .. } => None,
        }
    }
}

/// The sections as the filter `query` shows them: filterable sections
/// narrowed, the others whole.
pub(crate) fn filtered(sections: &[NavSection], query: &str) -> Vec<NavSection> {
    let query = crate::search::normalize_search_text(query.trim());
    if query.is_empty() {
        return sections.to_vec();
    }
    sections
        .iter()
        .map(|section| {
            if !section.filterable {
                return section.clone();
            }
            let mut narrowed = section.clone();
            narrowed.items = narrow(&section.items, &query);
            narrowed.collapsed = false;
            narrowed
        })
        .collect()
}

/// `items` narrowed to what matches `query` (folded), keeping the parents
/// of matching children, open. A row found only by one of its
/// [`NavItem::search_terms`] shows that term under its label.
pub(super) fn narrow(items: &[NavItem], query: &str) -> Vec<NavItem> {
    items
        .iter()
        .filter_map(|item| {
            let children = narrow(&item.children, query);
            let by_label = crate::search::normalize_search_text(&item.label).contains(query);
            let by_term = if by_label {
                None
            } else {
                item.search_terms.iter().find(|term| term.matches(query))
            };
            if children.is_empty() && !by_label && by_term.is_none() {
                return None;
            }
            let mut kept = item.clone();
            if let Some(name) = by_term.and_then(|term| term.name.clone())
                && kept.secondary.is_none()
            {
                kept.secondary = Some(name);
            }
            if !children.is_empty() {
                kept.ensure_expanded = true;
                kept.expanded = true;
            }
            kept.children = children;
            Some(kept)
        })
        .collect()
}

impl NavModel {
    pub(crate) fn row(&self, index: usize) -> Option<&NavRowData> {
        self.rows.get(index)
    }

    pub(crate) fn row_of(&self, key: &str) -> Option<&NavRowData> {
        self.by_key.get(key).and_then(|index| self.rows.get(*index))
    }

    /// The row whose place shows the resource `id`: the place bound to it (a channel's place
    /// shows the channel), or one whose key names it — a place key ends with the id of the
    /// resource it shows (`project:<id>`) or is the id itself.
    pub(crate) fn item_naming(&self, id: &str) -> Option<&str> {
        let shows = |row: &NavRowData| matches!(&row.kind, NavRowKind::Item { spec, .. } if spec.shows.as_deref() == Some(id));
        if let Some(row) = self.rows.iter().find(|row| shows(row))
            && let NavRowKind::Item { id: item, .. } = &row.kind
        {
            return Some(item.as_str());
        }
        self.rows.iter().find_map(|row| {
            let NavRowKind::Item { id: item, .. } = &row.kind else {
                return None;
            };
            let names = item == id
                || item
                    .strip_suffix(id)
                    .is_some_and(|rest| rest.ends_with(':') || rest.ends_with('/'));
            names.then_some(item.as_str())
        })
    }

    pub(crate) fn row_id(&self, id: &str) -> Option<usize> {
        self.index_of(&format!("item:{id}"))
            .or_else(|| self.index_of(&format!("section:{id}")))
    }

    /// Whether `key` is a destination.
    pub(crate) fn is_item(&self, key: &str) -> bool {
        matches!(
            self.row_of(key).map(|row| &row.kind),
            Some(NavRowKind::Item { .. })
        )
    }

    /// Whether `key` is a destination whose row is a checkbox.
    pub(crate) fn is_checkable(&self, key: &str) -> bool {
        matches!(
            self.row_of(key).map(|row| &row.kind),
            Some(NavRowKind::Item { spec, .. }) if spec.checked.is_some()
        )
    }

    /// Whether the person can rename `key` in place.
    pub(crate) fn is_renamable(&self, key: &str) -> bool {
        matches!(
            self.row_of(key).map(|row| &row.kind),
            Some(
                NavRowKind::Item { editable: true, .. }
                    | NavRowKind::Section { editable: true, .. }
            )
        )
    }

    pub(crate) fn shows_row_actions(&self, key: &str) -> bool {
        matches!(
            self.row_of(key).map(|row| &row.kind),
            Some(NavRowKind::Item { actions: true, .. })
        )
    }

    /// Whether `key`'s row can hold other items dropped into it.
    pub(crate) fn takes_children(&self, key: &str) -> bool {
        matches!(
            self.row_of(key).map(|row| &row.kind),
            Some(NavRowKind::Item {
                drop_target: true,
                ..
            })
        )
    }

    /// Whether `key`'s row shows a disclosure: a section that folds, or an
    /// item with children.
    pub(crate) fn is_branch(&self, key: &str) -> bool {
        matches!(
            self.row_of(key).map(|row| &row.kind),
            Some(
                NavRowKind::Section {
                    collapsible: true,
                    ..
                } | NavRowKind::Item { branch: true, .. }
            )
        )
    }

    /// Whether `key`'s children show.
    pub(crate) fn is_open(&self, key: &str) -> bool {
        matches!(
            self.row_of(key).map(|row| &row.kind),
            Some(NavRowKind::Section { open: true, .. } | NavRowKind::Item { open: true, .. })
        )
    }

    /// Whether `key` is a row the arrow keys stop on: a destination or a
    /// section header that folds. The empty section's caption and a pending
    /// row say something but do nothing.
    pub(crate) fn is_stop(&self, key: &str) -> bool {
        matches!(
            self.row_of(key).map(|row| &row.kind),
            Some(
                NavRowKind::Item { .. }
                    | NavRowKind::Section {
                        collapsible: true,
                        ..
                    }
            )
        )
    }

    /// The row the arrow keys stop on next from `from`, in the direction of
    /// travel: the arrow keys pass over captions, pending rows and section
    /// headers that do not fold.
    pub(crate) fn stop_from(&self, from: usize, forward: bool) -> Option<usize> {
        let stops = |index: &usize| self.is_stop(&self.rows[*index].key);
        if forward {
            (from + 1..self.rows.len()).find(stops)
        } else {
            (0..from.min(self.rows.len())).rev().find(stops)
        }
    }

    pub(crate) fn first_stop(&self) -> Option<usize> {
        (0..self.rows.len()).find(|index| self.is_stop(&self.rows[*index].key))
    }

    pub(crate) fn last_stop(&self) -> Option<usize> {
        (0..self.rows.len())
            .rev()
            .find(|index| self.is_stop(&self.rows[*index].key))
    }
}

/// The rows a [`NavList`](super::NavList) shows, in reading order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavModel {
    rows: Vec<NavRowData>,
    by_key: HashMap<String, usize>,
}

impl ListModel for NavModel {
    type Key = String;

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn key_at(&self, index: usize) -> Option<String> {
        self.rows.get(index).map(|row| row.key.clone())
    }

    fn index_of(&self, key: &String) -> Option<usize> {
        self.by_key.get(key).copied()
    }

    fn label(&self, index: usize) -> Option<String> {
        match &self.rows.get(index)?.kind {
            NavRowKind::Section { title, .. } => Some(title.clone()),
            NavRowKind::Item { label, .. } => Some(label.clone()),
            NavRowKind::Caption { caption, .. } => Some(caption.clone()),
            // What assistive technology hears of a row still being read.
            NavRowKind::Pending { .. } => Some("Loading".to_owned()),
        }
    }

    fn renames(&self, index: usize) -> bool {
        self.rows
            .get(index)
            .is_some_and(|row| self.is_renamable(&row.key))
    }

    /// Whether the row `index` takes other rows as its subtasks: a drop
    /// target offers a drop into it.
    fn accepts_children(&self, index: usize) -> bool {
        self.rows
            .get(index)
            .is_some_and(|row| self.takes_children(&row.key))
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct NavState {
    folded: HashSet<String>,
    opened: HashSet<String>,
    checks: HashMap<String, bool>,
}

impl NavState {
    /// Whether `key`'s children show: what the person did if they did
    /// anything, else what the app asked (`collapsed`, `expanded`), with the
    /// app's [`NavItem::ensure_expanded`] always open.
    pub(crate) fn open(&self, key: &str, app_open: bool, ensured: bool) -> bool {
        if ensured || self.opened.contains(key) {
            return true;
        }
        if self.folded.contains(key) {
            return false;
        }
        app_open
    }

    pub(crate) fn set_open(&mut self, key: &str, open: bool) {
        if open {
            self.folded.remove(key);
            self.opened.insert(key.to_owned());
        } else {
            self.opened.remove(key);
            self.folded.insert(key.to_owned());
        }
    }

    pub(crate) fn set_checked(&mut self, key: &str, checked: bool) {
        self.checks.insert(key.to_owned(), checked);
    }

    pub(crate) fn checked(&self, key: &str) -> Option<bool> {
        self.checks.get(key).copied()
    }
}

/// Build the rows `sections` show under `query`: filterable sections
/// narrowed, folded rows' children left out.
pub(crate) fn build(sections: &[NavSection], query: &str, state: &NavState) -> NavModel {
    let shown = filtered(sections, query);
    let mut model = NavModel::default();
    let filtering = !crate::search::normalize_search_text(query.trim()).is_empty();
    for section in &shown {
        let badges: Rc<Vec<String>> = Rc::new(
            section
                .items
                .iter()
                .filter_map(|item| item.badge.clone())
                .collect(),
        );
        let titled = !section.title.trim().is_empty();
        let open = !section.collapsible
            || filtering
            || state.open(
                &format!("section:{}", section.id),
                !section.collapsed,
                false,
            );
        if titled {
            let key = format!("section:{}", section.id);
            model.push(NavRowData {
                key,
                kind: NavRowKind::Section {
                    id: section.id.clone(),
                    title: section.title.clone(),
                    collapsible: section.collapsible,
                    open: open && section.collapsible,
                    action: section.trailing_action.as_ref().map(|action| action.icon),
                    action_label: section
                        .trailing_action
                        .as_ref()
                        .map(|action| action.label.clone()),
                    first: model.rows.is_empty(),
                    editable: section.editable_title,
                    menu: section.menu,
                },
            });
        }
        if section.items.is_empty() {
            if section.pending > 0 {
                if open {
                    for row in 0..section.pending {
                        model.push(NavRowData {
                            key: format!("pending:{}:{row}", section.id),
                            kind: NavRowKind::Pending {
                                id: section.id.clone(),
                            },
                        });
                    }
                }
            } else if let Some(caption) = &section.empty_caption
                && open
            {
                model.push(NavRowData {
                    key: format!("caption:{}", section.id),
                    kind: NavRowKind::Caption {
                        id: section.id.clone(),
                        caption: caption.clone(),
                    },
                });
            }
            continue;
        }
        if !open {
            continue;
        }
        for item in &section.items {
            model.push_item(item, 0, titled, &badges, state, filtering);
        }
    }
    model.reindex();
    model
}

impl NavModel {
    fn push(&mut self, row: NavRowData) {
        self.rows.push(row);
    }

    fn reindex(&mut self) {
        self.by_key = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.key.clone(), index))
            .collect();
    }

    /// Add one item of a section's tree, where it stands, its nested codes and the
    /// folds and checks the person made.
    fn push_item(
        &mut self,
        item: &NavItem,
        depth: usize,
        under_section: bool,
        badges: &Rc<Vec<String>>,
        state: &NavState,
        filtering: bool,
    ) {
        let key = format!("item:{}", item.id);
        let open = filtering || state.open(&key, item.expanded, item.ensure_expanded);
        let branch = !item.children.is_empty();
        let checked = state.checked(&key).or(item.checked);
        self.push(NavRowData {
            key: key.clone(),
            kind: NavRowKind::Item {
                id: item.id.clone(),
                label: item.label.clone(),
                spec: Box::new(NavItemSpec {
                    shows: item.resource.clone(),
                    under_section,
                    icon: item.icon,
                    count: item.count,
                    depth,
                    secondary: item.secondary.clone(),
                    meta: item.meta.clone(),
                    status: item.status.as_ref().map(|status| status.0.clone()),
                    status_label: item.status_label.clone(),
                    badge: item.badge.clone(),
                    section_badges: Rc::clone(badges),
                    swatch: item.swatch,
                    art: item.art.clone(),
                    checked,
                    level: item.level,
                    presence: item.presence.clone(),
                    person: item.person.clone(),
                    unread: item.unread,
                }),
                branch,
                open: branch && open,
                actions: item.actions,
                drop_target: item.drop_target,
                editable: item.editable_label,
                semantic_key: item.semantic_key.clone(),
            },
        });
        if branch && open {
            let child_badges: Rc<Vec<String>> = Rc::new(
                item.children
                    .iter()
                    .filter_map(|child| child.badge.clone())
                    .collect(),
            );
            for child in &item.children {
                self.push_item(
                    child,
                    depth.saturating_add(1),
                    under_section,
                    &child_badges,
                    state,
                    filtering,
                );
            }
        }
    }
}

impl NavSection {
    /// The id the section's `trailing_action` runs.
    pub(crate) fn action_id(&self) -> Option<&str> {
        self.trailing_action
            .as_ref()
            .map(|action| action.id.as_str())
    }
}
