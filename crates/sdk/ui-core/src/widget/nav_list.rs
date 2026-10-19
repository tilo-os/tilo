//! `NavList` — the one navigation list.
//!
//! Every sidebar in Tilo lists its destinations with a `NavList`: mailboxes,
//! channels, playlists, settings categories, places. An app says what the
//! destinations are ([`NavSection`], [`NavItem`]) and which one is chosen;
//! the list decides how they look. There is no colour, size, padding or
//! radius to pass.
//!
//! The list is the [`TreeView`] engine in its navigation presentation: arrow
//! keys move the selection, Left/Right collapse and expand, Enter activates,
//! F2 renames an editable label, Delete reports [`NavList::on_delete`], the
//! Menu key, a right click and a long press ask for the row's menu
//! ([`NavList::on_context_menu`]), rows publish their semantics, and
//! dragging an item onto a drop target moves it. Rows paint through
//! [`crate::chrome::paint_nav_row`], so a painted app that calls that
//! painter draws the same rows.
//!
//! Inside a [`super::NavShell`] the list is the sidebar's content: choosing a
//! destination in a narrow window closes the drawer (or pushes the content
//! screen) without the app doing anything, and the shell's filter field
//! narrows the sections that are [`NavSection::filterable`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use vello::kurbo::{Point, Rect};
use vello::peniko::Color;

use crate::LocalState;
use crate::chrome::NavCount;
use crate::icons::Icon;
use crate::local_state::LocalBinding;
use crate::reorder::{DropPlacement, DropTarget, ReorderDrop};
use crate::widget::tree_view::{
    NavItemSpec, NavPresentation, NavRowSpec, NavStatus, NodeId, TreeModel, TreeView,
};
use crate::widget::{AppModel, FilterCount, Filterable, ModelPath, RowStatus, Tone, TreeRowAction};

/// A group of destinations under a sentence-case title.
#[derive(Clone, Debug, PartialEq)]
pub struct NavSection {
    pub id: String,
    /// Sentence case ("Mailboxes", "On this device"). An empty title draws
    /// no header: the items stand at the top level.
    pub title: String,
    /// The person can fold the section away.
    pub collapsible: bool,
    /// The section starts folded.
    pub collapsed: bool,
    /// A trailing action on the header (usually "New …" with
    /// [`Icon::Plus`]); reported through [`NavList::on_section_action`].
    pub trailing_action: Option<NavAction>,
    /// The one quiet line the section shows while it has no items.
    pub empty_caption: Option<String>,
    /// The window's filter narrows this section's items.
    pub filterable: bool,
    pub items: Vec<NavItem>,
}

impl NavSection {
    #[must_use]
    pub fn new(id: impl Into<String>, title: impl Into<String>, items: Vec<NavItem>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            collapsible: false,
            collapsed: false,
            trailing_action: None,
            empty_caption: None,
            filterable: false,
            items,
        }
    }

    /// Items with no header above them.
    #[must_use]
    pub fn untitled(id: impl Into<String>, items: Vec<NavItem>) -> Self {
        Self::new(id, "", items)
    }

    #[must_use]
    pub fn collapsible(mut self) -> Self {
        self.collapsible = true;
        self
    }

    /// Fold the section when it first appears (it stays foldable).
    #[must_use]
    pub fn collapsed(mut self) -> Self {
        self.collapsible = true;
        self.collapsed = true;
        self
    }

    #[must_use]
    pub fn trailing_action(mut self, action: NavAction) -> Self {
        self.trailing_action = Some(action);
        self
    }

    /// What the section says while it holds nothing ("No drafts"): one
    /// sentence, in tertiary ink, where its first row would be.
    #[must_use]
    pub fn empty_caption(mut self, caption: impl Into<String>) -> Self {
        self.empty_caption = Some(caption.into());
        self
    }

    /// Let the window's filter field ([`super::NavShell::with_filter`])
    /// narrow this section's items. Sections that do not opt in (a list of
    /// connections above the tables being filtered) stay as they are.
    #[must_use]
    pub fn filterable(mut self) -> Self {
        self.filterable = true;
        self
    }
}

/// A command offered on a row or a section header: an icon, and the name it
/// is announced and tooltipped by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavAction {
    pub id: String,
    pub icon: Icon,
    /// Sentence case, naming what happens ("New playlist", "More").
    pub label: String,
    /// Pressing it asks for the row's menu ([`NavList::on_context_menu`])
    /// instead of reporting the action.
    pub opens_menu: bool,
}

impl NavAction {
    #[must_use]
    pub fn new(id: impl Into<String>, icon: Icon, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            icon,
            label: label.into(),
            opens_menu: false,
        }
    }

    /// The row's "More" action: pressing it opens the row's menu under the
    /// action, the same menu a right click opens.
    #[must_use]
    pub fn opens_context_menu(mut self) -> Self {
        self.opens_menu = true;
        self
    }
}

/// Something a [`NavItem`] is also found by: a thing it holds, by its name
/// and the other words for it, or words for the destination itself.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NavSearchTerm {
    /// The name shown under the row's label when this term found it.
    pub name: Option<String>,
    /// Other words that find it; a word matches a query at a word start.
    pub words: Vec<String>,
}

impl NavSearchTerm {
    /// A thing the destination holds, found by `name`.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            words: Vec::new(),
        }
    }

    /// Words that find the destination itself; the row shows nothing more.
    #[must_use]
    pub fn words(words: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            name: None,
            words: words.into_iter().map(Into::into).collect(),
        }
    }

    /// Also found by `words`.
    #[must_use]
    pub fn also(mut self, words: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.words.extend(words.into_iter().map(Into::into));
        self
    }

    /// Whether `query` (folded) finds this term: at the start of a word of
    /// its name or of one of its words, so "light" finds "Night light" and
    /// "light mode" but not "flight mode".
    fn matches(&self, query: &str) -> bool {
        self.name
            .iter()
            .chain(&self.words)
            .any(|text| matches_word_start(text, query))
    }
}

/// Whether `query` occurs in `text` (both folded) where a word starts.
fn matches_word_start(text: &str, query: &str) -> bool {
    let text = crate::search::normalize_search_text(text);
    let mut before: Option<char> = None;
    text.char_indices().any(|(at, here)| {
        let starts_word = before.is_none_or(|before| !before.is_alphanumeric());
        before = Some(here);
        starts_word && text.get(at..).is_some_and(|rest| rest.starts_with(query))
    })
}

/// One destination. Every part but the label is optional; see
/// `docs/ui-migration.md`, "Rows: what each part of a `NavItem` means".
#[derive(Clone, Debug, PartialEq)]
pub struct NavItem {
    /// Stable across rebuilds; selection, expansion and focus follow it.
    pub id: String,
    pub label: String,
    pub icon: Option<Icon>,
    pub count: Option<NavCount>,
    /// Show the list's item actions ([`NavList::item_actions`]) on this row
    /// on hover and focus.
    pub actions: bool,
    /// Nested destinations, indented one step.
    pub children: Vec<NavItem>,
    /// Starts expanded (only meaningful with children).
    pub expanded: bool,
    /// Opens even where the person folded it (the place the app just
    /// revealed).
    pub ensure_expanded: bool,
    /// Other items may be dragged into this one ([`NavList::on_move`]).
    pub drop_target: bool,
    /// The label can be renamed in place ([`NavList::on_rename`]).
    pub editable_label: bool,
    pub secondary: Option<String>,
    pub meta: Option<String>,
    pub status: Option<NavItemStatus>,
    pub status_label: Option<(String, Tone)>,
    pub badge: Option<String>,
    pub swatch: Option<Color>,
    pub checked: Option<bool>,
    pub level: Option<f32>,
    pub presence: Vec<String>,
    /// The key assistive technology and tests address the row by; the
    /// default is `nav:item:{id}`.
    pub semantic_key: Option<String>,
    /// What else the row is found by when the list is filtered: the things
    /// the destination holds ("Tap to click" on a touchpad page) and the
    /// words people use for them. A row found by a term that has a name
    /// shows that name under its label.
    pub search_terms: Vec<NavSearchTerm>,
}

/// A row's status dot, as [`NavItem::status`] keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NavItemStatus(NavStatus);

impl NavItemStatus {
    /// The status as the row paints it.
    #[must_use]
    pub fn row_status(&self) -> RowStatus<'_> {
        self.0.row_status()
    }
}

impl NavItem {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            count: None,
            actions: true,
            children: Vec::new(),
            expanded: false,
            ensure_expanded: false,
            drop_target: false,
            editable_label: false,
            secondary: None,
            meta: None,
            status: None,
            status_label: None,
            badge: None,
            swatch: None,
            checked: None,
            level: None,
            presence: Vec::new(),
            semantic_key: None,
            search_terms: Vec::new(),
        }
    }

    /// Also found, when the list is filtered, by `terms`: what the
    /// destination holds and the words people use for it.
    #[must_use]
    pub fn search_terms(mut self, terms: impl IntoIterator<Item = NavSearchTerm>) -> Self {
        self.search_terms = terms.into_iter().collect();
        self
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// A quiet count (items in a folder).
    #[must_use]
    pub fn count(mut self, count: u32) -> Self {
        self.count = Some(NavCount::Neutral(count));
        self
    }

    /// A count that wants attention (unread, mentions): the list's one
    /// filled badge, in the person's colour. Zero shows nothing.
    #[must_use]
    pub fn live_count(mut self, count: u32) -> Self {
        self.count = (count > 0).then_some(NavCount::Live(count));
        self
    }

    #[must_use]
    pub fn children(mut self, children: Vec<NavItem>) -> Self {
        self.children = children;
        self
    }

    #[must_use]
    pub fn expanded(mut self) -> Self {
        self.expanded = true;
        self
    }

    /// Open this item whenever the sections are handed over, even where the
    /// person folded it: the place the app is revealing (the database a
    /// connection just opened).
    #[must_use]
    pub fn ensure_expanded(mut self) -> Self {
        self.expanded = true;
        self.ensure_expanded = true;
        self
    }

    #[must_use]
    pub fn drop_target(mut self) -> Self {
        self.drop_target = true;
        self
    }

    #[must_use]
    pub fn editable_label(mut self) -> Self {
        self.editable_label = true;
        self
    }

    /// This row offers none of the list's item actions.
    #[must_use]
    pub fn without_actions(mut self) -> Self {
        self.actions = false;
        self
    }

    /// One `Caption` line under the label (a host and port, a schedule).
    /// It elides; the row grows to hold it.
    #[must_use]
    pub fn secondary(mut self, text: impl Into<String>) -> Self {
        self.secondary = Some(text.into());
        self
    }

    /// A tertiary figure at the trailing edge (a size).
    #[must_use]
    pub fn meta(mut self, text: impl Into<String>) -> Self {
        self.meta = Some(text.into());
        self
    }

    /// A status dot at the trailing edge: a drive's health, a connection's
    /// state, an account that needs attention, a person's presence.
    #[must_use]
    pub fn status(mut self, status: RowStatus<'_>) -> Self {
        self.status = Some(NavItemStatus(NavStatus::of(status)));
        self
    }

    /// A quiet tinted badge naming the row's state ("Active").
    #[must_use]
    pub fn status_label(mut self, label: impl Into<String>, tone: Tone) -> Self {
        self.status_label = Some((label.into(), tone));
        self
    }

    /// A short leading code that classifies the row (`GET`, `POST`). The
    /// codes of one section share a column, so their labels line up.
    #[must_use]
    pub fn badge(mut self, code: impl Into<String>) -> Self {
        self.badge = Some(code.into());
        self
    }

    /// The content colour the row stands for (a calendar, a project), in
    /// the icon's place.
    #[must_use]
    pub fn swatch(mut self, color: Color) -> Self {
        self.swatch = Some(color);
        self
    }

    /// A checkbox in the leading slot (a calendar shown or hidden). The
    /// whole row toggles it and reports through [`NavList::on_check`]; a
    /// checkable row is not a destination and is never selected.
    #[must_use]
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    /// A usage bar under the label, `fraction` of it filled (a backup
    /// destination's space).
    #[must_use]
    pub fn level(mut self, fraction: f32) -> Self {
        self.level = Some(fraction.clamp(0.0, 1.0));
        self
    }

    /// Someone is on this place: a mark in their presence colour. Call once
    /// per person.
    #[must_use]
    pub fn presence(mut self, identity: impl Into<String>) -> Self {
        self.presence.push(identity.into());
        self
    }

    /// The key assistive technology, agents and tests address the row by.
    #[must_use]
    pub fn semantic_key(mut self, key: impl Into<String>) -> Self {
        self.semantic_key = Some(key.into());
        self
    }
}

/// Where a dragged item lands (see [`NavList::on_move`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NavDrop {
    /// Into a drop target, as its last child.
    Into(String),
    /// Just before this item.
    Before(String),
    /// Just after this item.
    After(String),
}

#[derive(Clone, Debug, PartialEq)]
enum NavNodeKind {
    Root,
    Section {
        collapsible: bool,
        action: Option<(Icon, String)>,
        first: bool,
    },
    Item {
        spec: Box<NavItemSpec>,
        actions: bool,
        drop_target: bool,
        editable: bool,
        semantic_key: Option<String>,
    },
    /// An empty section's caption.
    Caption,
}

#[derive(Clone, Debug, PartialEq)]
struct NavNode {
    key: String,
    label: String,
    kind: NavNodeKind,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
}

/// The flattened sections and items a [`NavList`]'s tree reads. Node ids
/// are allocated per key once and reused across rebuilds, so the tree keeps
/// selection, expansion and focus when the app hands it new sections.
#[derive(Clone, Debug, Default)]
pub struct NavModel {
    nodes: HashMap<NodeId, NavNode>,
}

/// Stable node ids per key, kept across rebuilds.
#[derive(Debug, Default)]
struct NavKeys {
    by_key: HashMap<String, NodeId>,
    next: NodeId,
}

impl NavKeys {
    fn id(&mut self, key: &str) -> NodeId {
        if let Some(id) = self.by_key.get(key) {
            return *id;
        }
        self.next = self.next.saturating_add(1);
        self.by_key.insert(key.to_owned(), self.next);
        self.next
    }

    fn section(&self, id: &str) -> Option<NodeId> {
        self.by_key.get(&format!("section:{id}")).copied()
    }
}

const ROOT: NodeId = 0;

/// What a build of the model asks the tree to open.
#[derive(Default)]
struct Openings {
    /// Open as the app asked, unless the person folded it since.
    expanded: Vec<NodeId>,
    /// Open whatever the person did.
    ensured: Vec<NodeId>,
}

/// Whether `label` matches a filter `query` the way search matches.
fn matches_query(label: &str, query: &str) -> bool {
    crate::search::normalize_search_text(label).contains(query)
}

/// `items` narrowed to what matches `query` (folded), keeping the parents
/// of matching children, open. A row found only by one of its
/// [`NavItem::search_terms`] shows that term under its label.
fn narrow(items: &[NavItem], query: &str) -> Vec<NavItem> {
    items
        .iter()
        .filter_map(|item| {
            let children = narrow(&item.children, query);
            let by_label = matches_query(&item.label, query);
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

fn count_items(items: &[NavItem]) -> usize {
    items
        .iter()
        .map(|item| 1usize.saturating_add(count_items(&item.children)))
        .sum()
}

/// `sections` as the filter `query` shows them: filterable sections
/// narrowed, the others whole.
fn filtered(sections: &[NavSection], query: &str) -> Vec<NavSection> {
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

impl NavModel {
    fn build(sections: &[NavSection], keys: &mut NavKeys) -> (Self, Openings) {
        let mut model = Self::default();
        let mut openings = Openings::default();
        model.nodes.insert(
            ROOT,
            NavNode {
                key: String::new(),
                label: String::new(),
                kind: NavNodeKind::Root,
                parent: None,
                children: Vec::new(),
            },
        );
        let mut first_row = true;
        for section in sections {
            let parent = if section.title.trim().is_empty() {
                ROOT
            } else {
                let id = keys.id(&format!("section:{}", section.id));
                model.attach(
                    ROOT,
                    id,
                    NavNode {
                        key: section.id.clone(),
                        label: section.title.clone(),
                        kind: NavNodeKind::Section {
                            collapsible: section.collapsible,
                            action: section
                                .trailing_action
                                .as_ref()
                                .map(|action| (action.icon, action.label.clone())),
                            first: first_row,
                        },
                        parent: Some(ROOT),
                        children: Vec::new(),
                    },
                );
                if !section.collapsed {
                    openings.expanded.push(id);
                }
                id
            };
            first_row = false;
            if section.items.is_empty()
                && let Some(caption) = &section.empty_caption
            {
                let id = keys.id(&format!("caption:{}", section.id));
                model.attach(
                    parent,
                    id,
                    NavNode {
                        key: format!("caption:{}", section.id),
                        label: caption.clone(),
                        kind: NavNodeKind::Caption,
                        parent: Some(parent),
                        children: Vec::new(),
                    },
                );
            }
            let badges: Rc<Vec<String>> = Rc::new(
                section
                    .items
                    .iter()
                    .filter_map(|item| item.badge.clone())
                    .collect(),
            );
            for item in &section.items {
                model.add_item(parent, item, 0, &badges, keys, &mut openings);
            }
        }
        (model, openings)
    }

    fn add_item(
        &mut self,
        parent: NodeId,
        item: &NavItem,
        depth: usize,
        badges: &Rc<Vec<String>>,
        keys: &mut NavKeys,
        openings: &mut Openings,
    ) {
        let id = keys.id(&format!("item:{}", item.id));
        let under_section = self
            .nodes
            .get(&parent)
            .is_some_and(|node| match &node.kind {
                NavNodeKind::Section { .. } => true,
                NavNodeKind::Item { spec, .. } => spec.under_section,
                _ => false,
            });
        let spec = NavItemSpec {
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
            checked: item.checked,
            level: item.level,
            presence: item.presence.clone(),
        };
        self.attach(
            parent,
            id,
            NavNode {
                key: item.id.clone(),
                label: item.label.clone(),
                kind: NavNodeKind::Item {
                    spec: Box::new(spec),
                    actions: item.actions,
                    drop_target: item.drop_target,
                    editable: item.editable_label,
                    semantic_key: item.semantic_key.clone(),
                },
                parent: Some(parent),
                children: Vec::new(),
            },
        );
        if item.expanded {
            openings.expanded.push(id);
        }
        if item.ensure_expanded {
            openings.ensured.push(id);
        }
        // Nested codes line up among their siblings.
        let child_badges: Rc<Vec<String>> = Rc::new(
            item.children
                .iter()
                .filter_map(|child| child.badge.clone())
                .collect(),
        );
        for child in &item.children {
            self.add_item(
                id,
                child,
                depth.saturating_add(1),
                &child_badges,
                keys,
                openings,
            );
        }
    }

    fn attach(&mut self, parent: NodeId, id: NodeId, node: NavNode) {
        self.nodes.insert(id, node);
        if let Some(parent) = self.nodes.get_mut(&parent) {
            parent.children.push(id);
        }
    }

    fn key(&self, id: NodeId) -> Option<&str> {
        self.nodes.get(&id).map(|node| node.key.as_str())
    }

    fn item_id(&self, key: &str) -> Option<NodeId> {
        self.nodes.iter().find_map(|(id, node)| {
            (node.key == key && matches!(node.kind, NavNodeKind::Item { .. })).then_some(*id)
        })
    }

    fn section_id(&self, key: &str) -> Option<NodeId> {
        self.nodes.iter().find_map(|(id, node)| {
            (node.key == key && matches!(node.kind, NavNodeKind::Section { .. })).then_some(*id)
        })
    }

    fn is_item(&self, id: NodeId) -> bool {
        self.nodes
            .get(&id)
            .is_some_and(|node| matches!(node.kind, NavNodeKind::Item { .. }))
    }

    fn is_checkable(&self, id: NodeId) -> bool {
        self.nodes.get(&id).is_some_and(
            |node| matches!(&node.kind, NavNodeKind::Item { spec, .. } if spec.checked.is_some()),
        )
    }

    fn spec(&self, id: NodeId) -> Option<NavRowSpec> {
        let node = self.nodes.get(&id)?;
        match &node.kind {
            NavNodeKind::Root => None,
            NavNodeKind::Section {
                collapsible,
                action,
                first,
            } => Some(NavRowSpec::Section {
                title: node.label.clone(),
                collapsible: *collapsible,
                action: action.as_ref().map(|(icon, _)| *icon),
                action_label: action.as_ref().map(|(_, label)| label.clone()),
                first: *first,
            }),
            NavNodeKind::Item { spec, .. } => Some(NavRowSpec::Item(spec.clone())),
            NavNodeKind::Caption => Some(NavRowSpec::Caption),
        }
    }
}

impl TreeModel for NavModel {
    fn root_id(&self) -> NodeId {
        ROOT
    }

    fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    fn label(&self, id: NodeId) -> Option<&str> {
        self.nodes.get(&id).map(|node| node.label.as_str())
    }

    fn is_branch(&self, id: NodeId) -> bool {
        self.nodes.get(&id).is_some_and(|node| match node.kind {
            NavNodeKind::Root | NavNodeKind::Section { .. } => true,
            NavNodeKind::Item { .. } => !node.children.is_empty(),
            NavNodeKind::Caption => false,
        })
    }

    fn accepts_children(&self, id: NodeId) -> bool {
        self.nodes.get(&id).is_some_and(|node| {
            matches!(
                node.kind,
                NavNodeKind::Item {
                    drop_target: true,
                    ..
                }
            )
        })
    }

    fn semantic_key(&self, id: NodeId) -> Option<String> {
        let node = self.nodes.get(&id)?;
        match &node.kind {
            NavNodeKind::Root | NavNodeKind::Caption => None,
            NavNodeKind::Section { .. } => Some(format!("nav:section:{}", node.key)),
            NavNodeKind::Item { semantic_key, .. } => Some(
                semantic_key
                    .clone()
                    .unwrap_or_else(|| format!("nav:item:{}", node.key)),
            ),
        }
    }

    fn parent_id(&self, id: NodeId) -> Option<NodeId> {
        self.nodes.get(&id).and_then(|node| node.parent)
    }

    fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
        self.nodes
            .get(&parent)?
            .children
            .iter()
            .position(|id| *id == child)
    }

    fn child_count(&self, parent: NodeId) -> usize {
        self.nodes
            .get(&parent)
            .map_or(0, |node| node.children.len())
    }

    fn can_collapse(&self, id: NodeId) -> bool {
        // A section folds only when it says it does.
        self.nodes.get(&id).is_some_and(|node| match node.kind {
            NavNodeKind::Root | NavNodeKind::Caption => false,
            NavNodeKind::Section { collapsible, .. } => collapsible,
            NavNodeKind::Item { .. } => true,
        })
    }

    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
        self.nodes.get(&parent)?.children.get(index).copied()
    }

    fn is_selectable(&self, id: NodeId) -> bool {
        // A checkable row toggles; it is not a destination.
        self.is_item(id) && !self.is_checkable(id)
    }

    fn is_renamable(&self, id: NodeId) -> bool {
        self.nodes
            .get(&id)
            .is_some_and(|node| matches!(node.kind, NavNodeKind::Item { editable: true, .. }))
    }

    fn shows_row_actions(&self, id: NodeId) -> bool {
        self.nodes
            .get(&id)
            .is_some_and(|node| matches!(node.kind, NavNodeKind::Item { actions: true, .. }))
    }
}

type Callback<A> = Option<Box<dyn FnMut(A)>>;

/// What the tree's callbacks report into, shared with the closures the
/// tree holds.
#[derive(Default)]
struct NavShared {
    /// Node id → item or section id, refreshed on every rebuild.
    keys: HashMap<NodeId, String>,
    /// Item ids whose rows are checkable, with their state.
    checks: HashMap<NodeId, bool>,
    item_actions: Vec<String>,
    section_actions: HashMap<NodeId, String>,
    on_select: Callback<String>,
    selection_actions: HashMap<String, crate::widget::ActionEmitter<()>>,
    selection_emitted: bool,
    on_action: Option<Box<dyn FnMut(&str, &str)>>,
    on_section_action: Option<Box<dyn FnMut(&str, &str)>>,
    on_rename: Option<Box<dyn FnMut(&str, String)>>,
    on_move: Option<Box<dyn FnMut(&str, NavDrop)>>,
    on_check: Option<Box<dyn FnMut(&str, bool)>>,
    on_expand: Option<Box<dyn FnMut(&str, bool)>>,
    on_delete: Callback<String>,
    on_menu: Option<Box<dyn FnMut(&str, Rect, Point)>>,
    binding: Option<LocalBinding<Option<String>>>,
    /// The chosen key, including quiet app-driven reconciliation.
    selected: Option<String>,
    /// The person chose a destination since the shell last asked.
    navigated: bool,
}

/// The navigation list. See the module docs.
pub struct NavList {
    tree: TreeView<(), NavModel>,
    shared: Rc<RefCell<NavShared>>,
    keys: NavKeys,
    /// The sections as the app handed them, before any filter.
    sections: Vec<NavSection>,
    /// The filter the window's field applies to filterable sections.
    filter: String,
    /// What was open before a filter opened every match, restored when the
    /// filter clears.
    unfiltered_open: Option<Vec<NodeId>>,
    /// The app state the list follows, read at every sync and update.
    source: Option<NavSource>,
}

/// What a followed list reads: the sections and the chosen destination, or
/// `None` while the state cannot be read (it is borrowed elsewhere).
type NavSource = Box<dyn FnMut() -> Option<(Vec<NavSection>, Option<String>)>>;

impl NavList {
    /// Erase the widget type in the SDK, keeping the navigation tree's widget
    /// implementation in the shared runtime rather than in each thin app.
    /// The boxed widget still downcasts to `NavList`.
    #[must_use]
    pub fn into_widget(self) -> Box<dyn crate::widget::Widget> {
        Box::new(self)
    }

    #[must_use]
    pub fn new(sections: Vec<NavSection>) -> Self {
        let mut keys = NavKeys::default();
        let (model, openings) = NavModel::build(&sections, &mut keys);
        let shared = Rc::new(RefCell::new(NavShared::default()));
        let select_shared = Rc::clone(&shared);
        let activate_shared = Rc::clone(&shared);
        let action_shared = Rc::clone(&shared);
        let rename_shared = Rc::clone(&shared);
        let expand_shared = Rc::clone(&shared);
        let tree = TreeView::from_model(model)
            .hide_root()
            .fill()
            .with_expanded(openings.expanded)
            .selection_follows_focus(true)
            .click_activation(crate::widget::TreeClickActivation::SingleClick)
            .on_select(move |id| {
                let mut shared = select_shared.borrow_mut();
                let Some(key) = shared.keys.get(&id).cloned() else {
                    return;
                };
                if shared
                    .selection_actions
                    .get(&key)
                    .is_some_and(|action| !action.is_available())
                {
                    return;
                }
                shared.navigated = true;
                shared.selection_emitted = false;
                if shared.selected.as_deref() == Some(key.as_str()) {
                    return;
                }
                shared.selected = Some(key.clone());
                if let Some(binding) = shared.binding.as_mut() {
                    binding.publish(&Some(key.clone()));
                }
                if let Some(action) = shared.selection_actions.get_mut(&key) {
                    let _ = action.emit(());
                    shared.selection_emitted = true;
                } else if let Some(callback) = shared.on_select.as_mut() {
                    callback(key);
                }
            })
            .on_activate(move |id| {
                // Activating the selected destination still navigates to its
                // screen; selection callbacks remain change-only.
                let mut shared = activate_shared.borrow_mut();
                let Some(key) = shared.keys.get(&id).cloned() else {
                    return;
                };
                if shared
                    .selection_actions
                    .get(&key)
                    .is_some_and(|action| !action.is_available())
                {
                    return;
                }
                shared.navigated = true;
                if !std::mem::take(&mut shared.selection_emitted)
                    && let Some(action) = shared.selection_actions.get_mut(&key)
                {
                    let _ = action.emit(());
                }
            })
            .on_row_action(move |id, index| {
                let mut shared = action_shared.borrow_mut();
                let (Some(key), Some(action)) = (
                    shared.keys.get(&id).cloned(),
                    shared.item_actions.get(index).cloned(),
                ) else {
                    return;
                };
                if let Some(callback) = shared.on_action.as_mut() {
                    callback(&key, &action);
                }
            })
            .on_rename(move |id, title| {
                let mut shared = rename_shared.borrow_mut();
                let Some(key) = shared.keys.get(&id).cloned() else {
                    return;
                };
                if let Some(callback) = shared.on_rename.as_mut() {
                    callback(&key, title);
                }
            })
            .on_expand(move |id, open| {
                let mut shared = expand_shared.borrow_mut();
                let Some(key) = shared.keys.get(&id).cloned() else {
                    return;
                };
                if let Some(callback) = shared.on_expand.as_mut() {
                    callback(&key, open);
                }
            });
        let mut list = Self {
            tree,
            shared,
            keys,
            sections,
            filter: String::new(),
            unfiltered_open: None,
            source: None,
        };
        list.install_presentation(false);
        list.refresh_keys();
        list
    }

    /// Paint the tree as this list's rows (a rail's tiles when `rail`),
    /// reporting header actions, checks and menus into the list.
    fn install_presentation(&mut self, rail: bool) {
        // A toggled check shows at once, before the app's next sections
        // confirm it.
        let spec_shared = Rc::clone(&self.shared);
        let spec = move |model: &NavModel, id: NodeId| {
            let mut spec = model.spec(id)?;
            if let NavRowSpec::Item(item) = &mut spec
                && item.checked.is_some()
                && let Ok(shared) = spec_shared.try_borrow()
                && let Some(checked) = shared.checks.get(&id)
            {
                item.checked = Some(*checked);
            }
            Some(spec)
        };
        let mut nav = NavPresentation::new(rail, Box::new(spec));
        let section_shared = Rc::clone(&self.shared);
        nav.on_section_action = Some(Box::new(move |id| {
            let mut shared = section_shared.borrow_mut();
            let (Some(section), Some(action)) = (
                shared.keys.get(&id).cloned(),
                shared.section_actions.get(&id).cloned(),
            ) else {
                return;
            };
            if let Some(callback) = shared.on_section_action.as_mut() {
                callback(&section, &action);
            }
        }));
        let check_shared = Rc::clone(&self.shared);
        nav.on_check = Some(Box::new(move |id| {
            let mut shared = check_shared.borrow_mut();
            let (Some(key), Some(checked)) = (
                shared.keys.get(&id).cloned(),
                shared.checks.get(&id).copied(),
            ) else {
                return;
            };
            // The row shows the new state at once; the app's next sections
            // confirm it.
            shared.checks.insert(id, !checked);
            if let Some(callback) = shared.on_check.as_mut() {
                callback(&key, !checked);
            }
        }));
        if self.shared.borrow().on_menu.is_some() {
            nav.on_menu = Some(self.menu_reporter());
        }
        self.tree.set_nav_presentation(nav);
    }

    fn menu_reporter(&self) -> Box<dyn FnMut(NodeId, Rect, Point)> {
        let shared = Rc::clone(&self.shared);
        Box::new(move |id, lift, at| {
            let mut shared = shared.borrow_mut();
            let Some(key) = shared.keys.get(&id).cloned() else {
                return;
            };
            if let Some(callback) = shared.on_menu.as_mut() {
                callback(&key, lift, at);
            }
        })
    }

    fn refresh_keys(&self) {
        let mut shared = self.shared.borrow_mut();
        let model = self.tree.model();
        shared.keys = model
            .nodes
            .iter()
            .map(|(id, node)| (*id, node.key.clone()))
            .collect();
        shared.checks = model
            .nodes
            .iter()
            .filter_map(|(id, node)| match &node.kind {
                NavNodeKind::Item { spec, .. } => spec.checked.map(|checked| (*id, checked)),
                _ => None,
            })
            .collect();
        shared.section_actions = self
            .sections
            .iter()
            .filter_map(|section| {
                let action = section.trailing_action.as_ref()?;
                let id = self.keys.section(&section.id)?;
                Some((id, action.id.clone()))
            })
            .collect();
    }

    /// Give the list a stable id for tests and inspection.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.tree = self.tree.with_id(id);
        self
    }

    /// The name the list is announced by ("Mailboxes", "Settings").
    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.tree = self.tree.with_accessible_name(name);
        self
    }

    /// Choose `id` and bring its row into view. Does not report through
    /// [`Self::on_select`].
    #[must_use]
    pub fn selected(mut self, id: &str) -> Self {
        self.set_selected(Some(id));
        self
    }

    /// Keep the chosen destination in `state`: the list shows what the state
    /// holds (and brings it into view) and writes the person's choice back.
    #[must_use]
    pub fn bind_selection(mut self, state: &LocalState<Option<String>>) -> Self {
        let (value, binding) = LocalBinding::new(state);
        self.set_selected(value.as_deref());
        self.shared.borrow_mut().binding = Some(binding);
        self
    }

    /// Keep the list in step with app state: `source` answers the sections
    /// and the chosen destination, read now and at every sync and update. A
    /// choice the person makes shows until the state answers it; a state that
    /// chooses otherwise wins.
    #[must_use]
    pub fn follow(
        mut self,
        source: impl FnMut() -> Option<(Vec<NavSection>, Option<String>)> + 'static,
    ) -> Self {
        self.source = Some(Box::new(source));
        self.pull();
        self
    }

    /// Read the followed state; whether the list changed.
    fn pull(&mut self) -> bool {
        let Some((sections, selected)) = self.source.as_mut().and_then(|source| source()) else {
            return false;
        };
        let changed = sections != self.sections;
        self.set_sections(sections);
        if self.selected_id() == selected {
            return changed;
        }
        self.set_selected(selected.as_deref());
        true
    }

    /// Called with the item id when the person chooses a destination.
    #[must_use]
    pub fn on_select(self, f: impl FnMut(String) + 'static) -> Self {
        self.shared.borrow_mut().on_select = Some(Box::new(f));
        self
    }

    /// Bind each destination to its published typed action. Bound rows route
    /// selection through host custody; unbound rows keep `on_select` behavior.
    #[must_use]
    pub fn selection_actions(
        self,
        bindings: impl IntoIterator<Item = (String, tilo_actions::ActionBinding<()>)>,
    ) -> Self {
        self.shared.borrow_mut().selection_actions = bindings
            .into_iter()
            .map(|(id, binding)| (id, crate::widget::ActionEmitter::new(binding)))
            .collect();
        self
    }

    /// Actions shown on hover and focus at the trailing edge of every item
    /// that has not opted out ([`NavItem::without_actions`]); reported with
    /// the item id and the action id through `on_action`. An action that
    /// [opens the menu](NavAction::opens_context_menu) asks for the row's
    /// menu instead.
    #[must_use]
    pub fn item_actions(
        mut self,
        actions: Vec<NavAction>,
        on_action: impl FnMut(&str, &str) + 'static,
    ) -> Self {
        let row_actions = actions
            .iter()
            .map(|action| {
                let row = TreeRowAction::new(action.icon, action.label.clone());
                if action.opens_menu {
                    row.opens_context_menu()
                } else {
                    row
                }
            })
            .collect();
        {
            let mut shared = self.shared.borrow_mut();
            shared.item_actions = actions.into_iter().map(|action| action.id).collect();
            shared.on_action = Some(Box::new(on_action));
        }
        self.tree = self.tree.with_row_actions(row_actions);
        self
    }

    /// Called with the section id and the action id when a section header's
    /// trailing action is pressed.
    #[must_use]
    pub fn on_section_action(self, f: impl FnMut(&str, &str) + 'static) -> Self {
        self.shared.borrow_mut().on_section_action = Some(Box::new(f));
        self
    }

    /// Called with the item id and the new label when the person renames an
    /// editable item (F2 or a double click).
    #[must_use]
    pub fn on_rename(mut self, f: impl FnMut(&str, String) + 'static) -> Self {
        self.shared.borrow_mut().on_rename = Some(Box::new(f));
        self.tree = self.tree.rename_on_double_click();
        self
    }

    /// Called with the item id and its new state when the person toggles a
    /// checkable row ([`NavItem::checked`]): a click anywhere on it, or
    /// Space or Enter.
    #[must_use]
    pub fn on_check(self, f: impl FnMut(&str, bool) + 'static) -> Self {
        self.shared.borrow_mut().on_check = Some(Box::new(f));
        self
    }

    /// Called with the item or section id and whether it is now open when
    /// the person folds or unfolds it, so the app can keep what was folded.
    #[must_use]
    pub fn on_expand(self, f: impl FnMut(&str, bool) + 'static) -> Self {
        self.shared.borrow_mut().on_expand = Some(Box::new(f));
        self
    }

    /// Called with the focused item's id when the person presses Delete
    /// (never Backspace, which belongs to whatever editor sits beside the
    /// list).
    #[must_use]
    pub fn on_delete(mut self, f: impl FnMut(String) + 'static) -> Self {
        self.shared.borrow_mut().on_delete = Some(Box::new(f));
        let shared = Rc::clone(&self.shared);
        self.tree = self.tree.on_delete(move |id| {
            let mut shared = shared.borrow_mut();
            let Some(key) = shared.keys.get(&id).cloned() else {
                return;
            };
            if let Some(callback) = shared.on_delete.as_mut() {
                callback(key);
            }
        });
        self
    }

    /// Called when the person asks for an item's menu: a right click or a
    /// long press on its row, the Menu key or Shift+F10 on it, or an item
    /// action that [opens the menu](NavAction::opens_context_menu). It
    /// receives the item id, the row's lift (hand it to
    /// `ContextMenu::anchor_row` so the row stays marked) and where the menu
    /// opens: the pointer, or the bottom-leading corner of the row or the
    /// action.
    #[must_use]
    pub fn on_context_menu(mut self, f: impl FnMut(&str, Rect, Point) + 'static) -> Self {
        self.shared.borrow_mut().on_menu = Some(Box::new(f));
        let reporter = self.menu_reporter();
        if let Some(nav) = self.tree.nav_mut() {
            nav.on_menu = Some(reporter);
        }
        self
    }

    /// Let the person drag items: into an item marked
    /// [`NavItem::drop_target`], or before and after items in the same
    /// section. The app moves the item in its own data and hands the list
    /// its new sections ([`Self::set_sections`]).
    #[must_use]
    pub fn on_move(mut self, f: impl FnMut(&str, NavDrop) + 'static) -> Self {
        self.shared.borrow_mut().on_move = Some(Box::new(f));
        let shared = Rc::clone(&self.shared);
        self.tree = self.tree.with_reorder(
            |model: &NavModel, source: NodeId, target: &DropTarget<NodeId>| {
                if !model.is_item(source) || !model.is_item(target.key) {
                    return false;
                }
                match target.placement {
                    DropPlacement::Inside => model.accepts_children(target.key),
                    DropPlacement::Before | DropPlacement::After => true,
                }
            },
            move |model: &mut NavModel, dropped: ReorderDrop<NodeId>| {
                let (Some(source), Some(target)) = (
                    model.key(dropped.source).map(str::to_owned),
                    model.key(dropped.target.key).map(str::to_owned),
                ) else {
                    return false;
                };
                let drop = match dropped.target.placement {
                    DropPlacement::Inside => NavDrop::Into(target),
                    DropPlacement::Before => NavDrop::Before(target),
                    DropPlacement::After => NavDrop::After(target),
                };
                if let Some(callback) = shared.borrow_mut().on_move.as_mut() {
                    callback(&source, drop);
                }
                // The app owns the order; the list changes when it hands
                // back new sections.
                false
            },
        );
        self
    }

    /// An icon-over-label compact rail (a workspace switcher, a channel
    /// rail). Section titles give way to space; counts become a mark;
    /// captions wrap to a second line before they elide.
    #[must_use]
    pub fn rail(mut self) -> Self {
        self.install_presentation(true);
        self
    }

    /// Replace the destinations, keeping the selection, expansion and focus
    /// of every id that is still there.
    pub fn set_sections(&mut self, sections: Vec<NavSection>) {
        if sections == self.sections {
            return;
        }
        self.sections = sections;
        self.rebuild();
    }

    /// Build the tree's model from the sections as the filter shows them.
    fn rebuild(&mut self) {
        let selected = self.selected_id();
        let shown = filtered(&self.sections, &self.filter);
        let (model, openings) = NavModel::build(&shown, &mut self.keys);
        let kept: Vec<NodeId> = self.tree.expanded_ids().collect();
        let ensured: HashSet<NodeId> = openings.ensured.iter().copied().collect();
        // What the person folded stays folded; what is new opens as the app
        // asked, a section that does not fold is always open, and what the
        // app ensures opens whatever the person did.
        let open: Vec<NodeId> = openings
            .expanded
            .into_iter()
            .filter(|id| !self.keys_seen(*id) || !model.can_collapse(*id) || ensured.contains(id))
            .chain(kept)
            .chain(openings.ensured)
            .collect();
        self.tree
            .update_source(&[crate::widget::TreeModelChange::Reset], move |current| {
                *current = model;
            });
        self.tree.restore_expanded(open);
        self.refresh_keys();
        self.select_quietly(selected.as_deref());
    }

    fn keys_seen(&self, id: NodeId) -> bool {
        self.shared.borrow().keys.contains_key(&id)
    }

    /// Choose `id` (or nothing) without reporting it, and bring its row
    /// into view (once the list has a viewport, if it has none yet).
    pub fn set_selected(&mut self, id: Option<&str>) {
        let node = self.select_quietly(id);
        if let Some(node) = node {
            self.tree.reveal(node);
        }
    }

    fn select_quietly(&mut self, id: Option<&str>) -> Option<NodeId> {
        let node = id.and_then(|key| self.tree.model().item_id(key));
        match node {
            Some(node) => self.tree.selection_mut().select_only(node),
            None => self.tree.selection_mut().clear(),
        }
        if let Some(node) = node {
            self.tree.set_focused(Some(node));
        }
        self.shared.borrow_mut().selected = node
            .and_then(|node| self.tree.model().key(node))
            .map(str::to_owned);
        node
    }

    /// The chosen destination.
    #[must_use]
    pub fn selected_id(&self) -> Option<String> {
        let id = self.tree.selected()?;
        self.tree.model().key(id).map(str::to_owned)
    }

    /// Open or fold item or section `id` without reporting it.
    pub fn set_expanded(&mut self, id: &str, open: bool) {
        let model = self.tree.model();
        let Some(node) = model.item_id(id).or_else(|| model.section_id(id)) else {
            return;
        };
        let mut expanded: Vec<NodeId> = self.tree.expanded_ids().collect();
        if open {
            expanded.push(node);
        } else {
            expanded.retain(|kept| *kept != node);
        }
        self.tree.restore_expanded(expanded);
    }

    /// Whether item or section `id` is open.
    #[must_use]
    pub fn is_expanded(&self, id: &str) -> bool {
        let model = self.tree.model();
        model
            .item_id(id)
            .or_else(|| model.section_id(id))
            .is_some_and(|node| self.tree.is_expanded(node))
    }

    /// The painted rect of item `id`'s row, edge to edge of the list, when
    /// it is in view.
    #[must_use]
    pub fn row_rect(&self, id: &str) -> Option<Rect> {
        let node = self.tree.model().item_id(id)?;
        self.tree.row_rect(node)
    }

    /// The lift of item `id`'s row: where its hover and selection paint.
    /// Hand it to `ContextMenu::anchor_row` so a menu marks the row.
    #[must_use]
    pub fn row_lift_rect(&self, id: &str) -> Option<Rect> {
        let node = self.tree.model().item_id(id)?;
        self.tree.nav_lift_rect(node)
    }

    /// Where item action `action` sits on item `id`'s row (whether or not
    /// the row is hovered), to open a menu at it with `open_anchored`.
    #[must_use]
    pub fn item_action_rect(&self, id: &str, action: &str) -> Option<Rect> {
        let node = self.tree.model().item_id(id)?;
        let index = self
            .shared
            .borrow()
            .item_actions
            .iter()
            .position(|known| known == action)?;
        self.tree.row_action_rect(node, index)
    }

    /// Show item `id` being dragged to just before item `before`, for a
    /// capture: the row lifts under a resting pointer and the drop line
    /// shows. Nothing moves unless the person releases it. Needs
    /// [`Self::on_move`].
    pub fn stage_drag(&mut self, id: &str, before: &str) {
        let model = self.tree.model();
        if let (Some(source), Some(target)) = (model.item_id(id), model.item_id(before)) {
            self.tree.stage_reorder(source, target);
        }
    }

    /// Narrow the filterable sections to `query` (an empty query shows
    /// everything). The shell's filter field calls this; an app with its own
    /// field may too.
    pub fn set_filter(&mut self, query: &str) {
        if query == self.filter {
            return;
        }
        let was_filtering = !self.filter.trim().is_empty();
        let filtering = !query.trim().is_empty();
        if filtering && !was_filtering {
            self.unfiltered_open = Some(self.tree.expanded_ids().collect());
        }
        query.clone_into(&mut self.filter);
        self.rebuild();
        if !filtering && let Some(open) = self.unfiltered_open.take() {
            self.tree.restore_expanded(open);
        }
    }

    /// The filter the list is narrowed to.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Stable item ids in the current expanded and filtered row order.
    /// Section headers are omitted. Inspection reads the same rows that the
    /// retained navigation tree renders and exposes to accessibility.
    #[must_use]
    pub fn visible_item_ids(&self) -> Vec<String> {
        let model = self.tree.model();
        let shared = self.shared.borrow();
        self.tree
            .visible_rows()
            .into_iter()
            .filter_map(|(node, _, _)| {
                let key = shared.keys.get(&node)?;
                (model.item_id(key) == Some(node)).then(|| key.clone())
            })
            .collect()
    }

    /// The list's tree, for tests reading its engine state.
    #[cfg(test)]
    pub(crate) fn tree(&self) -> &TreeView<(), NavModel> {
        &self.tree
    }
}

impl Filterable for NavList {
    fn set_filter(&mut self, query: &str) {
        Self::set_filter(self, query);
    }

    fn filter_count(&self) -> Option<FilterCount> {
        let filterable: Vec<&NavSection> = self
            .sections
            .iter()
            .filter(|section| section.filterable)
            .collect();
        if filterable.is_empty() {
            return None;
        }
        let total = filterable
            .iter()
            .map(|section| count_items(&section.items))
            .sum();
        let shown = filtered(&self.sections, &self.filter)
            .iter()
            .filter(|section| section.filterable)
            .map(|section| count_items(&section.items))
            .sum();
        Some(FilterCount::new(shown, total))
    }
}

/// Lets the shell hosting a list learn that the person just chose a
/// destination, so a narrow window can move on to the content.
pub(crate) trait NavigationSource {
    fn take_navigation(&mut self) -> bool;
}

/// A rail whose width follows its destinations' words
/// ([`crate::chrome::rail_width_fitting`]).
pub(crate) trait RailExtent {
    /// How wide this rail is under `theme`, or `None` when it is no rail.
    fn fitted_rail_width(&self, theme: &crate::Theme) -> Option<f64>;
}

impl RailExtent for NavList {
    fn fitted_rail_width(&self, theme: &crate::Theme) -> Option<f64> {
        if !self.tree.is_rail() {
            return None;
        }
        fn labels<'a>(items: &'a [NavItem], out: &mut Vec<&'a str>) {
            for item in items {
                out.push(&item.label);
                labels(&item.children, out);
            }
        }
        let mut all = Vec::new();
        for section in &self.sections {
            labels(&section.items, &mut all);
        }
        Some(crate::text_render::with_layout_renderer(
            theme.text_scale,
            |text| crate::chrome::rail_width_fitting(theme, text, all.iter().copied()),
        ))
    }
}

/// The width of the first rail in `widget`'s subtree under `theme`
/// ([`RailExtent`]), or `None` where it holds none.
pub(crate) fn fitted_rail_width(
    widget: &dyn crate::widget::Widget,
    theme: &crate::Theme,
) -> Option<f64> {
    if let Some(rail) = crate::widget::capability::capability::<dyn RailExtent>(widget) {
        return rail.fitted_rail_width(theme);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| fitted_rail_width(child, theme))
}

impl NavigationSource for NavList {
    fn take_navigation(&mut self) -> bool {
        std::mem::take(&mut self.shared.borrow_mut().navigated)
    }
}

/// Whether anything in `widget`'s subtree reported a navigation since the
/// last ask (and forget it).
pub(crate) fn take_navigation(widget: &mut dyn crate::widget::Widget) -> bool {
    if let Some(source) = crate::widget::capability::capability_mut::<dyn NavigationSource>(widget)
    {
        return source.take_navigation();
    }
    let mut found = false;
    for child in widget.children_mut() {
        found |= take_navigation(child);
    }
    found
}

// Not the blanket `Decorator`: apps box a navigation list, and its Widget
// methods (with the tree view they forward to) are compiled once, here
// (`concrete_decorator_widget!` in decorator.rs).
impl crate::widget::decorator::concrete::Decorator for NavList {
    type Inner = TreeView<(), NavModel>;

    fn inner(&self) -> &Self::Inner {
        &self.tree
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.tree
    }

    fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn crate::widget::controls::ActionControls>() {
            request.provide::<dyn crate::widget::controls::ActionControls>(self);
            return;
        }
        if request.wants::<dyn RailExtent>() {
            request.provide::<dyn RailExtent>(self);
            return;
        }
        crate::widget::Widget::provide(&self.tree, request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn crate::widget::controls::ActionControls>() {
            request.provide::<dyn crate::widget::controls::ActionControls>(self);
            return;
        }
        if request.wants::<dyn NavigationSource>() {
            request.provide::<dyn NavigationSource>(self);
            return;
        }
        if request.wants::<dyn Filterable>()
            && self.sections.iter().any(|section| section.filterable)
        {
            request.provide::<dyn Filterable>(self);
            return;
        }
        crate::widget::Widget::provide_mut(&mut self.tree, request);
    }

    fn build_layout(
        &mut self,
        taffy: &mut crate::taffy::TaffyTree<()>,
        theme: &crate::Theme,
        node_map: &mut HashMap<crate::taffy::NodeId, *mut dyn crate::widget::Widget>,
    ) -> crate::taffy::NodeId {
        let node = crate::widget::Widget::build_layout(&mut self.tree, taffy, theme, node_map);
        // The published owner is NavList, so host lookup and synthetic
        // accessibility routing must identify this same retained wrapper.
        node_map.insert(node, std::ptr::from_mut::<dyn crate::widget::Widget>(self));
        node
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let next = self
            .shared
            .borrow_mut()
            .binding
            .as_mut()
            .and_then(LocalBinding::poll);
        if let Some(next) = next {
            self.set_selected(next.as_deref());
        }
        self.pull();
        crate::widget::Widget::sync(&mut self.tree, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let pulled = self.pull();
        crate::widget::Widget::update(&mut self.tree, state, path) || pulled
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        fn refresh(node: &mut crate::a11y::AccessNode, shared: &NavShared) {
            if let Some(id) = node
                .action_target
                .as_deref()
                .and_then(|target| target.strip_prefix("node:"))
                .and_then(|id| id.parse::<NodeId>().ok())
                && let Some(key) = shared.keys.get(&id)
                && let Some(action) = shared.selection_actions.get(key)
            {
                node.state.disabled |= !action.is_available();
            }
            for child in &mut node.children {
                refresh(child, shared);
            }
        }
        let mut access = crate::widget::Widget::accessibility(&self.tree)?;
        refresh(&mut access, &self.shared.borrow());
        Some(access)
    }

    fn name(&self) -> &'static str {
        "NavList"
    }
}

#[cfg(test)]
mod tests;

impl crate::widget::controls::ActionControls for NavList {
    fn install_control_custody(
        &mut self,
        domain: &crate::ControlDomain,
        attachment: crate::control_custody::CustodyAttachment,
    ) {
        for action in self.shared.borrow_mut().selection_actions.values_mut() {
            let _ = action.install_custody(domain, attachment);
        }
        crate::widget::controls::install_control_custody(&mut self.tree, domain, attachment);
    }

    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.shared
            .borrow_mut()
            .selection_actions
            .values_mut()
            .find_map(crate::widget::ActionEmitter::take)
            .or_else(|| crate::widget::controls::take_control_invocation(&mut self.tree))
    }

    fn control_action_bindings(&self) -> Vec<crate::widget::ControlBindingMetadata> {
        let shared = self.shared.borrow();
        let mut bindings = crate::widget::controls::control_action_bindings(&self.tree);
        // The tree exports only the current paint window. Keep its control
        // inventory equally bounded, including after filtering or scrolling.
        for (node, _, _) in self
            .tree
            .visible_row_range()
            .filter_map(|index| self.tree.visible_row(index))
        {
            let Some(key) = shared.keys.get(&node) else {
                continue;
            };
            if let Some(action) = shared.selection_actions.get(key) {
                let id = format!("node:{node}");
                bindings.retain(|binding| binding.control_id != id);
                bindings.push(action.metadata(id));
            }
        }
        bindings
    }

    fn refresh_control_action_availability(
        &mut self,
        allowed: &HashSet<tilo_actions::ActionAddress>,
    ) {
        for action in self.shared.borrow_mut().selection_actions.values_mut() {
            action.refresh_availability(allowed);
        }
        crate::widget::controls::refresh_control_action_availability(&mut self.tree, allowed);
    }
}
