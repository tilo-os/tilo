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
//! F2 renames an editable label, rows publish their semantics, and dragging
//! an item onto a drop target moves it. Rows paint through
//! [`crate::chrome::paint_nav_row`], so a painted app that calls that
//! painter draws the same rows.
//!
//! Inside a [`super::NavShell`] the list is the sidebar's content: choosing a
//! destination in a narrow window closes the drawer (or pushes the content
//! screen) without the app doing anything.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::LocalState;
use crate::chrome::NavCount;
use crate::icons::Icon;
use crate::local_state::LocalBinding;
use crate::reorder::{DropPlacement, DropTarget, ReorderDrop};
use crate::widget::tree_view::{NavPresentation, NavRowSpec, NodeId, TreeModel, TreeView};
use crate::widget::{AppModel, ModelPath, TreeRowAction};

/// A group of destinations under a sentence-case title.
#[derive(Clone, Debug, PartialEq)]
pub struct NavSection {
    pub id: String,
    /// Sentence case ("Mailboxes", "On this device"). An empty title draws
    /// no header: the items stand at the top level.
    pub title: String,
    /// The person can fold the section away.
    pub collapsible: bool,
    /// A trailing action on the header (usually "New …" with
    /// [`Icon::Plus`]); reported through [`NavList::on_section_action`].
    pub trailing_action: Option<NavAction>,
    pub items: Vec<NavItem>,
}

impl NavSection {
    #[must_use]
    pub fn new(id: impl Into<String>, title: impl Into<String>, items: Vec<NavItem>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            collapsible: false,
            trailing_action: None,
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

    #[must_use]
    pub fn trailing_action(mut self, action: NavAction) -> Self {
        self.trailing_action = Some(action);
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
}

impl NavAction {
    #[must_use]
    pub fn new(id: impl Into<String>, icon: Icon, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            icon,
            label: label.into(),
        }
    }
}

/// One destination.
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
    /// Other items may be dragged into this one ([`NavList::on_move`]).
    pub drop_target: bool,
    /// The label can be renamed in place ([`NavList::on_rename`]).
    pub editable_label: bool,
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
            drop_target: false,
            editable_label: false,
        }
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
        action: Option<Icon>,
        first: bool,
    },
    Item {
        icon: Option<Icon>,
        count: Option<NavCount>,
        depth: usize,
        actions: bool,
        drop_target: bool,
        editable: bool,
    },
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
}

const ROOT: NodeId = 0;

impl NavModel {
    fn build(sections: &[NavSection], keys: &mut NavKeys) -> (Self, Vec<NodeId>) {
        let mut model = Self::default();
        let mut expanded = Vec::new();
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
                            action: section.trailing_action.as_ref().map(|action| action.icon),
                            first: first_row,
                        },
                        parent: Some(ROOT),
                        children: Vec::new(),
                    },
                );
                expanded.push(id);
                id
            };
            first_row = false;
            for item in &section.items {
                model.add_item(parent, item, 0, keys, &mut expanded);
            }
        }
        (model, expanded)
    }

    fn add_item(
        &mut self,
        parent: NodeId,
        item: &NavItem,
        depth: usize,
        keys: &mut NavKeys,
        expanded: &mut Vec<NodeId>,
    ) {
        let id = keys.id(&format!("item:{}", item.id));
        self.attach(
            parent,
            id,
            NavNode {
                key: item.id.clone(),
                label: item.label.clone(),
                kind: NavNodeKind::Item {
                    icon: item.icon,
                    count: item.count,
                    depth,
                    actions: item.actions,
                    drop_target: item.drop_target,
                    editable: item.editable_label,
                },
                parent: Some(parent),
                children: Vec::new(),
            },
        );
        if item.expanded {
            expanded.push(id);
        }
        for child in &item.children {
            self.add_item(id, child, depth.saturating_add(1), keys, expanded);
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

    fn is_item(&self, id: NodeId) -> bool {
        self.nodes
            .get(&id)
            .is_some_and(|node| matches!(node.kind, NavNodeKind::Item { .. }))
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
                action: *action,
                first: *first,
            }),
            NavNodeKind::Item {
                icon, count, depth, ..
            } => Some(NavRowSpec::Item {
                icon: *icon,
                count: *count,
                depth: *depth,
            }),
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
        match node.kind {
            NavNodeKind::Root => None,
            NavNodeKind::Section { .. } => Some(format!("nav:section:{}", node.key)),
            NavNodeKind::Item { .. } => Some(format!("nav:item:{}", node.key)),
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
            NavNodeKind::Root => false,
            NavNodeKind::Section { collapsible, .. } => collapsible,
            NavNodeKind::Item { .. } => true,
        })
    }

    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
        self.nodes.get(&parent)?.children.get(index).copied()
    }

    fn is_selectable(&self, id: NodeId) -> bool {
        self.is_item(id)
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
    item_actions: Vec<String>,
    section_actions: HashMap<NodeId, String>,
    on_select: Callback<String>,
    on_action: Option<Box<dyn FnMut(&str, &str)>>,
    on_section_action: Option<Box<dyn FnMut(&str, &str)>>,
    on_rename: Option<Box<dyn FnMut(&str, String)>>,
    on_move: Option<Box<dyn FnMut(&str, NavDrop)>>,
    binding: Option<LocalBinding<Option<String>>>,
    /// The person chose a destination since the shell last asked.
    navigated: bool,
}

/// The navigation list. See the module docs.
pub struct NavList {
    tree: TreeView<(), NavModel>,
    shared: Rc<RefCell<NavShared>>,
    keys: NavKeys,
    sections: Vec<NavSection>,
}

impl NavList {
    #[must_use]
    pub fn new(sections: Vec<NavSection>) -> Self {
        let mut keys = NavKeys::default();
        let (model, expanded) = NavModel::build(&sections, &mut keys);
        let shared = Rc::new(RefCell::new(NavShared::default()));
        let select_shared = Rc::clone(&shared);
        let action_shared = Rc::clone(&shared);
        let rename_shared = Rc::clone(&shared);
        let mut tree = TreeView::from_model(model)
            .hide_root()
            .fill()
            .with_expanded(expanded)
            .selection_follows_focus(true)
            .click_activation(crate::widget::TreeClickActivation::SingleClick)
            .on_select(move |id| {
                let mut shared = select_shared.borrow_mut();
                let Some(key) = shared.keys.get(&id).cloned() else {
                    return;
                };
                shared.navigated = true;
                if let Some(binding) = shared.binding.as_mut() {
                    binding.publish(&Some(key.clone()));
                }
                if let Some(callback) = shared.on_select.as_mut() {
                    callback(key);
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
            });
        let section_shared = Rc::clone(&shared);
        let mut nav = NavPresentation::new(false, Box::new(NavModel::spec));
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
        tree.set_nav_presentation(nav);
        let list = Self {
            tree,
            shared,
            keys,
            sections,
        };
        list.refresh_keys();
        list
    }

    fn refresh_keys(&self) {
        let mut shared = self.shared.borrow_mut();
        shared.keys = self
            .tree
            .model()
            .nodes
            .iter()
            .map(|(id, node)| (*id, node.key.clone()))
            .collect();
        shared.section_actions = self
            .sections
            .iter()
            .filter_map(|section| {
                let action = section.trailing_action.as_ref()?;
                let id = self.keys.by_key.get(&format!("section:{}", section.id))?;
                Some((*id, action.id.clone()))
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

    /// Choose `id`. Does not report through [`Self::on_select`].
    #[must_use]
    pub fn selected(mut self, id: &str) -> Self {
        self.set_selected(Some(id));
        self
    }

    /// Keep the chosen destination in `state`: the list shows what the state
    /// holds and writes the person's choice back.
    #[must_use]
    pub fn bind_selection(mut self, state: &LocalState<Option<String>>) -> Self {
        let (value, binding) = LocalBinding::new(state);
        self.set_selected(value.as_deref());
        self.shared.borrow_mut().binding = Some(binding);
        self
    }

    /// Called with the item id when the person chooses a destination.
    #[must_use]
    pub fn on_select(self, f: impl FnMut(String) + 'static) -> Self {
        self.shared.borrow_mut().on_select = Some(Box::new(f));
        self
    }

    /// Actions shown on hover and focus at the trailing edge of every item
    /// that has not opted out ([`NavItem::without_actions`]); reported with
    /// the item id and the action id through `on_action`.
    #[must_use]
    pub fn item_actions(
        mut self,
        actions: Vec<NavAction>,
        on_action: impl FnMut(&str, &str) + 'static,
    ) -> Self {
        let row_actions = actions
            .iter()
            .map(|action| TreeRowAction::new(action.icon, action.label.clone()))
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
    /// rail). Section titles give way to space; counts become a mark.
    #[must_use]
    pub fn rail(mut self) -> Self {
        let mut nav = NavPresentation::new(true, Box::new(NavModel::spec));
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
        self.tree.set_nav_presentation(nav);
        self
    }

    /// Replace the destinations, keeping the selection, expansion and focus
    /// of every id that is still there.
    pub fn set_sections(&mut self, sections: Vec<NavSection>) {
        if sections == self.sections {
            return;
        }
        let selected = self.selected_id();
        let (model, expanded) = NavModel::build(&sections, &mut self.keys);
        let kept: Vec<NodeId> = self.tree.expanded_ids().collect();
        // What the person folded stays folded; what is new opens as the app
        // asked, and a section that does not fold is always open.
        let open: Vec<NodeId> = expanded
            .into_iter()
            .filter(|id| !self.keys_seen(*id) || !model.can_collapse(*id))
            .chain(kept)
            .collect();
        self.tree
            .update_source(&[crate::widget::TreeModelChange::Reset], move |current| {
                *current = model
            });
        self.tree.restore_expanded(open);
        self.sections = sections;
        self.refresh_keys();
        self.set_selected(selected.as_deref());
    }

    fn keys_seen(&self, id: NodeId) -> bool {
        self.shared.borrow().keys.contains_key(&id)
    }

    /// Choose `id` (or nothing) without reporting it.
    pub fn set_selected(&mut self, id: Option<&str>) {
        let node = id.and_then(|key| self.tree.model().item_id(key));
        match node {
            Some(node) => self.tree.selection_mut().select_only(node),
            None => self.tree.selection_mut().clear(),
        }
        if let Some(node) = node {
            self.tree.set_focused(Some(node));
        }
    }

    /// The chosen destination.
    #[must_use]
    pub fn selected_id(&self) -> Option<String> {
        let id = self.tree.selected()?;
        self.tree.model().key(id).map(str::to_owned)
    }

    /// The painted rect of item `id`'s row, when it is in view.
    #[must_use]
    pub fn row_rect(&self, id: &str) -> Option<vello::kurbo::Rect> {
        let node = self.tree.model().item_id(id)?;
        self.tree.row_rect(node)
    }

    /// The list's tree, for tests reading its engine state.
    #[cfg(test)]
    pub(crate) fn tree(&self) -> &TreeView<(), NavModel> {
        &self.tree
    }
}

/// Lets the shell hosting a list learn that the person just chose a
/// destination, so a narrow window can move on to the content.
pub(crate) trait NavigationSource {
    fn take_navigation(&mut self) -> bool;
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

impl crate::widget::Decorator for NavList {
    type Inner = TreeView<(), NavModel>;

    fn inner(&self) -> &Self::Inner {
        &self.tree
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.tree
    }

    fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
        crate::widget::Widget::provide(&self.tree, request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn NavigationSource>() {
            request.provide::<dyn NavigationSource>(self);
            return;
        }
        crate::widget::Widget::provide_mut(&mut self.tree, request);
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let next = self
            .shared
            .borrow_mut()
            .binding
            .as_mut()
            .and_then(LocalBinding::poll);
        if let Some(next) = next {
            self.set_selected(next.as_deref());
        }
        crate::widget::Widget::sync(&mut self.tree, state, path);
    }

    fn name(&self) -> &'static str {
        "NavList"
    }
}

#[cfg(test)]
mod tests;
