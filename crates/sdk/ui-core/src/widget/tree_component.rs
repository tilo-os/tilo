//! `TreeView` as a described component: a tree of nodes, each a part in a slot, with its
//! children named under it by key, and selecting or activating a node reported with the
//! resource it stands for.
//!
//! The nodes are [`TreeViewNodeProps`], one per row, each naming its parent by the resource it
//! stands for, and the rows are drawn by the list's own tree
//! ([`VirtualList::with_tree`](crate::widget::VirtualList::with_tree)): a node that changed is
//! one part beside the others, and the tree changes in place — an expanded branch folds where
//! it stands, and the selection follows what the view says. Folding, keyboard navigation,
//! focus, type-ahead, the row's menu and its drag all come from the list, so a tree here
//! behaves as any other list of rows does.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::component::{Component, Described, Event, ResourceId, Value};
use crate::icons::Icon;
use crate::reorder::{DropPlacement, DropTarget, ReorderDrop};
use crate::selection::KeySelection;
use crate::widget::{
    Decorator, Element, ListModel, ListRow, MenuItem, MenuItemProps, Press, TreeMarks, TreeRow,
    VirtualList, Widget,
};

use crate::widget::virtual_list::by_hand;

/// The height of one row of a tree: an outline's rows are its own token, the compact row's
/// height, shorter than a list's row of the same theme.
pub const TREE_ROW_HEIGHT: f32 = crate::tokens::Density::Compact.rows().single;

/// What one row of a tree shows besides its label: a leading glyph, the tone its title reads
/// in, trailing text and what assistive technology says of it.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "TreeViewRow", role = "Generic")]
pub struct TreeViewRowProps {
    /// A leading glyph: the kind of thing it is. Beside a checkbox it stands after the box,
    /// so a checked-off task still says what it is.
    pub icon: Option<Icon>,
    /// The tone its label reads in.
    pub tone: Option<TreeTone>,
    /// Dim text after the label: what the row belongs to (a project, a folder).
    pub secondary: Option<String>,
    /// Trailing text: a count, a kind, a size.
    pub trailing: Option<String>,
    /// How far the branch got while it is folded, just before the trailing text: a done
    /// count among its children ("2/5"). Its node's [`TreeViewNodeProps::progress`] says it
    /// where the row gives none. Shown only when the branch is closed.
    pub progress: Option<String>,
    /// A checkbox at the start of the row: a task's done state, a selection. A press on it
    /// asks the node's [`TreeViewNodeProps::check`] instead of selecting the row.
    pub checkbox: Option<bool>,
    /// What assistive technology says of the row after its label ("Folder", "View").
    pub description: Option<String>,
}

/// The label's tone in a tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TreeTone {
    /// Body ink.
    #[default]
    Primary,
    /// Secondary ink, for dense or subordinate rows.
    Secondary,
}

crate::component::choice!(TreeTone, "TreeTone", {
    Primary => "primary",
    Secondary => "secondary",
});

/// One node of a tree: its label, the resource it stands for, and where it stands.
#[derive(Component, Default)]
#[component(
    name = "TreeViewNode",
    role = "TreeItem",
    label = "label",
    presents = "item"
)]
pub struct TreeViewNodeProps {
    /// The resource the node stands for: its stable key, what selecting or activating it
    /// reports, and what its children name as their parent.
    #[component(required, positional)]
    pub item: ResourceId,
    /// What the row is called.
    #[component(required, positional)]
    pub label: String,
    /// The resource of the node it stands under; without one it is a top-level node.
    pub parent: Option<ResourceId>,
    /// The node the view shows just over this one under the same parent: the place the actions
    /// offered on it take when one reads a place (`above`). None for a first child.
    pub above: Option<ResourceId>,
    /// Whether it opens to show the nodes under it. A node with children says so unless it
    /// says otherwise.
    pub branch: bool,
    /// What the row shows besides its label.
    #[component(slot)]
    pub row: Option<TreeViewRowProps>,
    /// How far the branch got while it is folded ("1/3"), which the owner counts of its own
    /// children: the same count names a list row and an outline node, so both say one thing.
    /// The row's own [`TreeViewRowProps::progress`] says it instead where the row gives one.
    pub progress: Option<String>,
    /// Whether it is the selected node.
    pub selected: bool,
    /// A node was dropped just before this one, or moved here by the keyboard: carries the
    /// node that moved, so the owner places it before this one under the same parent.
    #[component(event(value = ResourceId))]
    pub drop: Event,
    /// A node was dropped inside this one, at the end of its children, or indented here by
    /// the keyboard: carries the node that moved.
    #[component(event(value = ResourceId))]
    pub into: Event,
    /// This node's checkbox was pressed: the owner toggles what the box stands for.
    #[component(event)]
    pub check: Event,
    /// The commands of its context menu: a right-click, the Menu key or a long press opens it,
    /// so every action a row has stays reachable without a drag.
    #[component(slot)]
    pub menu: Vec<MenuItemProps>,
}

crate::component::part_slot!(TreeViewNodeProps);
crate::component::part_slot!(TreeViewRowProps);

/// A tree of nodes: a file explorer, a project's files, the layers of a drawing, a book's
/// pages. Each node is the resource it stands for; opening a branch shows the nodes under it,
/// and selecting or activating a node reports its resource.
#[derive(Component, Default)]
#[component(name = "TreeView", role = "Tree", label = "name")]
pub struct TreeViewProps {
    /// What the tree is, for assistive technology.
    pub name: Option<String>,
    /// The tree takes the height it is given and scrolls inside itself, rather than sizing to
    /// the rows inside another scroll.
    pub fill: bool,
    /// The tree's root node — the tree itself, labelled with its `name` — is not a row: the
    /// nodes at the top level are, at the first depth. Off, the tree's name stands over them
    /// as a row of its own. Hide it where a heading or a page title already names the tree.
    pub hide_root: bool,
    /// A press selects and activates in one, as a navigator sidebar does; a double click
    /// activates otherwise.
    pub single_click: bool,
    /// Moving through the rows with the keyboard selects them as it goes.
    pub selection_follows_focus: bool,
    /// Whether several nodes may be selected at once (Ctrl-click, Shift-click).
    pub multi_select: bool,
    /// Rows move: by dragging one onto another (before it, inside it or after it), and by
    /// `Tab` and `Shift-Tab` to indent and outdent, `Alt-Up` and `Alt-Down` to reorder among
    /// siblings. A move is reported by the node it lands at
    /// ([`TreeViewNodeProps::drop`], [`TreeViewNodeProps::into`]) or, at the top level's end,
    /// by the tree's [`Self::drop`].
    pub reorder: bool,
    /// `Tab` and `Shift-Tab` indent and outdent the focused row. On by itself with
    /// [`Self::reorder`]; off, `Tab` moves the focus on as it otherwise would.
    pub tab_indents: bool,
    /// The nodes, in order, parents before their children.
    #[component(slot(default))]
    pub nodes: Vec<TreeViewNodeProps>,
    /// The nodes open now, by the resource each stands for. Given, it is where the view says
    /// the tree is; without it the tree keeps what a person opened.
    #[component(state)]
    pub expanded: Option<Vec<ResourceId>>,
    /// The nodes selected, by the resource each stands for.
    #[component(state)]
    pub selection: Option<Vec<ResourceId>>,
    /// The pace its own motion plays at: branches unfolding, rows arriving and leaving. Unset, the
    /// theme's.
    pub motion: Option<crate::motion::Feel>,
    /// A node was selected: carries the resource it shows.
    #[component(event(value = ResourceId))]
    pub select: Event,
    /// A node was activated (a double click, Enter, an assistive technology): carries the
    /// resource it shows.
    #[component(event(value = ResourceId))]
    pub activate: Event,
    /// A node was dropped at the end of the top level, past every row: carries the node that
    /// moved.
    #[component(event(value = ResourceId))]
    pub drop: Event,
}

/// A described tree: its list, the rows it was built from, and what it keeps of them.
pub struct DescribedTreeView {
    /// The widget id the tree was made with, kept so a change builds it again under the same
    /// id.
    key: String,
    list: VirtualList<Nodes>,
    /// What the rows are: every node, its parent and its row's parts, read once.
    nodes: Nodes,
    /// What the list works out of the rows as it mounts them, and what the person folded.
    marks: Rc<TreeMarks<String>>,
}

/// The key of the top level: the row of the tree's own name, over the nodes without a parent.
const TOP_LEVEL: &str = "";

/// One row of a tree, its parts read once.
struct Node {
    key: String,
    label: String,
    /// The row it stands under, as the list sees it: the tree's own row over a top-level node,
    /// or none where the tree hides that row.
    parent: Option<String>,
    row: Option<TreeViewRowProps>,
    progress: Option<String>,
    menu: Vec<Command>,
    drop: Event,
    into: Event,
    check: Event,
}

/// Every node of a tree, and what a list of them answers.
///
/// Cheap to clone: a row builder, a menu and a move all read the same rows.
#[derive(Clone)]
struct Nodes {
    inner: Rc<Inner>,
}

/// A tree's rows are read through the one copy of them a list and its row builder share.
impl std::ops::Deref for Nodes {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.inner
    }
}

struct Inner {
    nodes: Vec<Node>,
    index: HashMap<String, usize>,
    /// The rows standing under each row, in the order the props gave them; the top level's
    /// stand under [`TOP_LEVEL`].
    children: HashMap<String, Vec<String>>,
    /// The rows that open to show subtasks.
    branches: HashSet<String>,
    /// The resource each node stands for, by its key, and the key each resource stands at.
    items: HashMap<String, ResourceId>,
    keys: HashMap<String, String>,
    /// Whether the tree's own name is a row of the tree.
    root_shown: bool,
    /// A node was dropped at the end of the top level.
    root_drop: Event,
    /// Whether any node offers a menu, which the list offers of its rows only then.
    has_menu: bool,
}

/// One node of a tree, its parts read once.
struct Part {
    key: String,
    item: ResourceId,
    label: String,
    parent: Option<String>,
    branch: bool,
    row: Option<TreeViewRowProps>,
    progress: Option<String>,
    menu: Vec<MenuItemProps>,
    drop: Event,
    into: Event,
    check: Event,
}

/// One command of a node's context menu, kept so the menu can be offered each time it opens.
struct Command {
    label: String,
    icon: Option<Icon>,
    destructive: bool,
    checked: Option<bool>,
    key: Option<String>,
    group: Option<String>,
    enabled: bool,
    reason: Option<String>,
    /// What choosing the row presses: the action the view bound to it, or the view's own event.
    press: Press,
}

impl Command {
    fn of(props: MenuItemProps) -> Self {
        let MenuItemProps {
            label,
            icon,
            destructive,
            checked,
            disabled,
            reason,
            group,
            key,
            choose,
        } = props;
        Self {
            label,
            icon,
            destructive,
            checked,
            key,
            group,
            enabled: !disabled,
            reason,
            press: Press::of(choose),
        }
    }

    fn entry(&self) -> MenuItem {
        let run = self.press.runner();
        let item = if self.destructive {
            MenuItem::destructive_action(self.label.clone(), move || run())
        } else {
            MenuItem::action(self.label.clone(), move || run())
        }
        .for_press(self.press.clone());
        let item = match self.icon {
            Some(icon) => item.with_icon(icon),
            None => item,
        };
        let item = match self.checked {
            Some(checked) => item.with_checked(checked),
            None => item,
        };
        let item = item.with_enabled(self.enabled);
        let item = match &self.key {
            Some(key) => item.with_shortcut(key.clone()),
            None => item,
        };
        let item = match &self.reason {
            Some(reason) => item.with_description(reason.clone()),
            None => item,
        };
        match &self.group {
            Some(group) => item.with_group(group.clone()),
            None => item,
        }
    }
}

impl Nodes {
    /// The rows of `props`: the tree's own name over the nodes when it shows one, then every
    /// node, each naming the row it stands under.
    fn of(
        props: Vec<TreeViewNodeProps>,
        hide_root: bool,
        name: Option<String>,
        root_drop: Event,
    ) -> Self {
        let mut parts = Vec::with_capacity(props.len());
        for node in props {
            parts.push(Part {
                key: node.item.0.clone(),
                item: node.item,
                label: node.label,
                parent: node.parent.map(|parent| parent.0),
                branch: node.branch,
                row: node.row,
                progress: node.progress,
                menu: node.menu,
                drop: node.drop,
                into: node.into,
                check: node.check,
            });
        }
        let keys: HashSet<String> = parts.iter().map(|part| part.key.clone()).collect();
        let has_menu = parts.iter().any(|part| !part.menu.is_empty());
        let root_shown = !hide_root;
        let mut nodes: Vec<Node> = Vec::with_capacity(parts.len() + 1);
        let mut index = HashMap::new();
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        let mut branches = HashSet::new();
        let mut items = HashMap::new();
        let mut item_keys = HashMap::new();
        if root_shown {
            index.insert(TOP_LEVEL.to_owned(), 0);
            children.entry(TOP_LEVEL.to_owned()).or_default();
            branches.insert(TOP_LEVEL.to_owned());
            nodes.push(Node {
                key: TOP_LEVEL.to_owned(),
                label: name.unwrap_or_default(),
                parent: None,
                row: None,
                progress: None,
                menu: Vec::new(),
                drop: Event::None,
                into: Event::None,
                check: Event::None,
            });
        }
        // Where each node stands, read as the props give them: a parent the tree does not hold,
        // or the node itself, leaves it at the top level.
        let mut parents: HashMap<String, Option<String>> = HashMap::new();
        for part in parts {
            if index.contains_key(&part.key) {
                continue;
            }
            let parent = match part.parent {
                Some(parent) if keys.contains(&parent) && parent != part.key => Some(parent),
                _ => None,
            };
            // Two people moving a node into each other's subtree at the same moment can leave
            // links that close a loop. The node that comes last in the order does not follow
            // its link and stands at the top level, so the tree holds every node once and shows
            // the same thing to everyone.
            let parent = parent.filter(|parent| {
                let mut at = Some(parent.clone());
                for _ in 0..=keys.len() {
                    match at {
                        Some(step) if step == part.key => return false,
                        Some(step) => at = parents.get(&step).cloned().flatten(),
                        None => return true,
                    }
                }
                false
            });
            parents.insert(part.key.clone(), parent.clone());
            if part.branch {
                // A branch shows its disclosure whether or not its nodes are in the list: a
                // folder read when it opens.
                branches.insert(part.key.clone());
            }
            let under = parent.clone().unwrap_or_else(|| TOP_LEVEL.to_owned());
            children.entry(under).or_default().push(part.key.clone());
            index.insert(part.key.clone(), nodes.len());
            nodes.push(Node {
                key: part.key.clone(),
                label: part.label,
                parent: if root_shown {
                    Some(parent.unwrap_or_else(|| TOP_LEVEL.to_owned()))
                } else {
                    parent
                },
                row: part.row,
                progress: part.progress,
                menu: part.menu.into_iter().map(Command::of).collect(),
                drop: part.drop,
                into: part.into,
                check: part.check,
            });
            item_keys.insert(part.item.0.clone(), part.key.clone());
            items.insert(part.key, part.item);
        }
        // A row with nodes under it opens them, whether or not it says it is a branch.
        let mut with_children = HashSet::new();
        for (under, rows) in &children {
            if rows.is_empty() {
                continue;
            }
            if under == TOP_LEVEL && root_shown {
                continue;
            }
            with_children.insert(under.clone());
        }
        branches.extend(with_children);
        Self {
            inner: Rc::new(Inner {
                nodes,
                index,
                children,
                branches,
                items,
                keys: item_keys,
                root_shown,
                root_drop,
                has_menu,
            }),
        }
    }

    fn node(&self, key: &str) -> Option<&Node> {
        self.index.get(key).and_then(|at| self.nodes.get(*at))
    }

    /// The key of the node standing for the resource `id`.
    fn key_of_item(&self, id: &str) -> Option<String> {
        self.keys.get(id).cloned()
    }

    /// The resource the node at `key` stands for, as an event carries it.
    fn value_of(&self, key: &str) -> Option<Value> {
        self.items
            .get(key)
            .map(|item| Value::Resource(item.0.clone()))
    }

    /// The kind of node the rows ask for.
    #[cfg(test)]
    fn resource_of(&self, key: &str) -> Option<&ResourceId> {
        self.items.get(key)
    }

    /// The rows standing under `parent`, the top level's under `None`.
    fn siblings_of(&self, parent: Option<&str>) -> &[String] {
        self.children
            .get(parent.unwrap_or(TOP_LEVEL))
            .map_or(&[], Vec::as_slice)
    }

    /// The row a node stands under, among the nodes the owner names: none at the top level.
    fn declared_parent(&self, key: &str) -> Option<String> {
        self.node(key)
            .and_then(|node| node.parent.clone())
            .filter(|parent| parent != TOP_LEVEL)
    }

    /// Which rows start folded, given the rows the view says it holds open: every branch but
    /// those, and never the tree's own row, which holds the whole tree.
    fn folded_at_first(&self, expanded: &[ResourceId]) -> Vec<String> {
        let open: HashSet<String> = expanded
            .iter()
            .filter_map(|id| self.key_of_item(&id.0))
            .collect();
        self.branches
            .iter()
            .filter(|key| key.as_str() != TOP_LEVEL && !open.contains(*key))
            .cloned()
            .collect()
    }

    /// Whether `source` may land at `target`: not on itself, not inside its own subtree, and
    /// inside only a row that takes subtasks.
    fn can_reorder(&self, source: &String, target: &DropTarget<String>) -> bool {
        // The tree's own row stands for nothing to move.
        if target.key == *source || !self.items.contains_key(source) {
            return false;
        }
        if matches!(target.placement, DropPlacement::Inside) && !self.branches.contains(&target.key)
        {
            return false;
        }
        let mut at = match target.placement {
            DropPlacement::Inside => Some(target.key.clone()),
            DropPlacement::Before | DropPlacement::After => self.declared_parent(&target.key),
        };
        while let Some(key) = at {
            if key == *source {
                return false;
            }
            at = self.declared_parent(&key);
        }
        true
    }

    /// Where a dragged or keyed move lands, reported by the node it lands at: the node it goes
    /// before, the node it goes inside of, or the end of the top level. Returns whether the
    /// owner was told.
    fn report(&self, dropped: &ReorderDrop<String>) -> bool {
        let Some(value) = self.value_of(&dropped.source) else {
            return false;
        };
        // The tree's own row, or a row the list does not hold: the top level's end.
        let target = if (self.root_shown && dropped.target.key == TOP_LEVEL)
            || !self.index.contains_key(&dropped.target.key)
        {
            None
        } else {
            Some(&dropped.target)
        };
        let (parent, next) = match target {
            Some(target) => match target.placement {
                DropPlacement::Inside => (
                    Some(target.key.clone()),
                    self.siblings_of(Some(&target.key)).first().cloned(),
                ),
                DropPlacement::Before => {
                    (self.declared_parent(&target.key), Some(target.key.clone()))
                }
                DropPlacement::After => {
                    let parent = self.declared_parent(&target.key);
                    let siblings = self.siblings_of(parent.as_deref());
                    let next = siblings
                        .iter()
                        .position(|sibling| sibling == &target.key)
                        .and_then(|at| at.checked_add(1))
                        .and_then(|at| siblings.get(at))
                        .cloned();
                    (parent, next)
                }
            },
            None => (None, None),
        };
        if let Some(next) = next {
            let Some(event) = self.node(&next).map(|node| node.drop.clone()) else {
                return false;
            };
            event.fire(Some(value));
            return true;
        }
        if let Some(parent) = parent {
            let Some(event) = self.node(&parent).map(|node| node.into.clone()) else {
                return false;
            };
            event.fire(Some(value));
            return true;
        }
        if self.root_drop.is_none() {
            return false;
        }
        self.root_drop.fire(Some(value));
        true
    }

    /// The commands of the row's menu, as the list offers them.
    fn menu_of(&self, key: Option<&String>) -> Vec<MenuItem> {
        key.and_then(|key| self.node(key))
            .map(|node| node.menu.iter().map(Command::entry).collect())
            .unwrap_or_default()
    }
}

impl ListModel for Nodes {
    type Key = String;

    fn len(&self) -> usize {
        self.nodes.len()
    }

    fn key_at(&self, index: usize) -> Option<Self::Key> {
        self.nodes.get(index).map(|node| node.key.clone())
    }

    fn index_of(&self, key: &Self::Key) -> Option<usize> {
        self.index.get(key).copied()
    }

    fn tree_at(&self, index: usize) -> Option<TreeRow<Self::Key>> {
        Some(TreeRow {
            parent: self.nodes.get(index)?.parent.clone(),
        })
    }

    fn accepts_children(&self, index: usize) -> bool {
        self.has_subtasks(index)
    }

    fn has_subtasks(&self, index: usize) -> bool {
        self.nodes
            .get(index)
            .is_some_and(|node| self.branches.contains(&node.key))
    }

    fn discloses(&self) -> bool {
        true
    }

    /// The row's name, which typing its first letters moves to.
    fn label(&self, index: usize) -> Option<String> {
        self.nodes.get(index).map(|node| node.label.clone())
    }

    /// The row showing the resource `id`, for a host that renders rows as they show.
    fn key_presenting(&self, id: &str) -> Option<Self::Key> {
        self.key_of_item(id)
    }
}

/// How far the branch under `node` got, said only while it is folded: open, its own rows say
/// it. The row's own wording stands over the count its node states.
fn progress_of(node: &Node, folded: bool) -> Option<String> {
    let row = node.row.as_ref()?;
    if !folded {
        return None;
    }
    row.progress.clone().or_else(|| node.progress.clone())
}

/// The row of the node at `key`: its list row, indented to where it stands, with what its row
/// part says of it.
fn row_of(
    nodes: &Nodes,
    key: &str,
    feel: crate::motion::Feel,
    marks: &TreeMarks<String>,
) -> Element {
    let Some(node) = nodes.node(key) else {
        return Box::new(ListRow::new(key.to_owned(), ""));
    };
    let key_owned = key.to_owned();
    let folded = marks.is_folded(&key_owned);
    let mut row = ListRow::new(key_owned.clone(), node.label.clone())
        .depth(marks.depth(&key_owned))
        .row_height(TREE_ROW_HEIGHT)
        .with_feel(feel);
    if let Some(props) = &node.row {
        // The box leads, and the glyph after it: a task checks off and still says what it is.
        if let Some(checked) = props.checkbox {
            let check = node.check.clone();
            row = match props.icon {
                Some(icon) => row.checkbox_with_icon(checked, icon, move |_| check.fire(None)),
                None => row.checkbox(checked, move |_| check.fire(None)),
            };
        } else if let Some(icon) = props.icon {
            row = row.icon(icon);
        }
        if props.tone == Some(TreeTone::Secondary) {
            row = row.quiet();
        }
        if let Some(text) = &props.secondary {
            row = row.secondary(text.clone());
        }
        if let Some(text) = &props.trailing {
            row = row.value(text.clone());
        }
        if let Some(progress) = progress_of(node, folded) {
            row = row.meta(progress);
        }
        if let Some(text) = &props.description {
            row = row.description(text.clone());
        }
    }
    Box::new(row)
}

impl DescribedTreeView {
    /// The rows the tree was built from.
    #[cfg(test)]
    fn nodes(&self) -> &Nodes {
        &self.nodes
    }

    /// Whether the node at `key` is open.
    #[cfg(test)]
    pub(crate) fn is_open(&self, key: &str) -> bool {
        !self.marks.is_folded(&key.to_owned())
    }

    /// The key of the row standing for the resource `item`, which is also the node's stable
    /// name on screen.
    #[cfg(test)]
    pub(crate) fn row_key(&self, item: &str) -> Option<String> {
        self.nodes.key_of_item(item)
    }

    /// The rows selected, by key.
    #[cfg(test)]
    pub(crate) fn selected(&self) -> Vec<String> {
        self.list.selection().stored_keys().cloned().collect()
    }

    /// The resource the node at `key` stands for.
    #[cfg(test)]
    pub(crate) fn item_of(&self, key: &str) -> Option<&ResourceId> {
        self.nodes.resource_of(key)
    }

    /// The tree of `props`: its rows, and what it reports of them.
    fn build(key: &str, props: TreeViewProps) -> Self {
        let TreeViewProps {
            name,
            fill,
            hide_root,
            single_click,
            selection_follows_focus,
            multi_select,
            reorder,
            tab_indents,
            nodes,
            expanded,
            selection,
            motion,
            select,
            activate,
            drop,
        } = props;
        let feel = motion.unwrap_or(crate::motion::Feel::Standard);
        let nodes = Nodes::of(nodes, hide_root, name.clone(), drop);
        // What the rows of the tree are: the list fills it as it mounts them, and reads it as
        // it builds each one.
        let marks = Rc::new(TreeMarks::<String>::default());
        // A view that says which rows it holds open is where the tree is, every build; one that
        // says nothing leaves what a person folded to the list.
        if let Some(open) = expanded.as_deref() {
            marks.restore_folded(nodes.folded_at_first(open));
        }
        let row_marks = Rc::clone(&marks);
        let mut list = VirtualList::with_selection_delegate(
            nodes.clone(),
            TREE_ROW_HEIGHT,
            move |nodes, _, key, _| row_of(nodes, key, feel, &row_marks),
        )
        .with_tree(Rc::clone(&marks))
        .with_id(key)
        // A node's target is its resource: the same node keeps it while its row moves.
        .with_semantic_key(|key: &String| format!("node:{key}"));
        if let Some(name) = &name {
            list = list.with_accessible_name(name.clone());
        }
        // A tree inside another scroll is as tall as its rows; one that fills a pane takes its
        // height and scrolls them itself.
        list = if fill {
            list.filling()
        } else {
            list.fitting_rows()
        };
        if selection_follows_focus {
            list = list.selection_follows_focus();
        }
        if multi_select {
            list = list.multi_select(true);
        }
        if single_click {
            list = list.open_on_press();
        }
        // Each event carries the resource its row shows.
        if !select.is_none() || !activate.is_none() {
            let reporting = nodes.clone();
            let picking = nodes.clone();
            let select_single = single_click;
            let selected = select.clone();
            list = list
                .on_select(move |key, chosen| {
                    if !chosen {
                        return;
                    }
                    if let Some(value) = reporting.value_of(&key) {
                        selected.fire(Some(value));
                    }
                })
                .on_activate(move |key| {
                    let value = picking.value_of(&key);
                    // Opening a row is one act: the selection it moved there on the way is
                    // reported as the choice it is, as a press on a navigator's row is.
                    if select_single && by_hand() {
                        select.fire(value.clone());
                    }
                    activate.fire(value);
                });
        }
        if nodes.has_menu {
            let menus = nodes.clone();
            list = list.with_context_menu(
                move |_: &Nodes, key: Option<&String>| menus.menu_of(key),
                |_, _| {},
            );
        }
        if reorder {
            let moves = nodes.clone();
            list = list.with_reorder(
                |nodes: &Nodes, source: &String, target: &DropTarget<String>| {
                    nodes.can_reorder(source, target)
                },
                move |_: &mut Nodes, dropped: ReorderDrop<String>| {
                    // The owner moves it; the rows show what the model then says.
                    moves.report(&dropped);
                    false
                },
            );
            if tab_indents {
                list = list.tab_indents(true);
            }
        }
        if let Some(selected) = &selection {
            let mut chosen = KeySelection::new();
            for id in selected {
                if let Some(key) = nodes.key_of_item(&id.0) {
                    chosen.toggle(key);
                }
            }
            list = list.with_selection(chosen);
        }
        Self {
            key: key.to_owned(),
            list,
            nodes,
            marks,
        }
    }
}

impl Described for DescribedTreeView {
    type Props = TreeViewProps;

    fn from_props(key: &str, props: TreeViewProps) -> Self {
        Self::build(key, props)
    }

    /// The rows are built again with what the person kept: a branch they folded stays folded,
    /// what they selected stays selected, the row they were on stays focused and where they
    /// were in it stays. A change of a node's label or place is a row the list draws, so
    /// nothing narrower saves the rebuild.
    fn set_props(&mut self, props: TreeViewProps) -> bool {
        let motion = props.motion;
        let folded = self.marks.folded_keys();
        let selected: Vec<String> = self.list.selection().stored_keys().cloned().collect();
        let focused = self.list.focused_key().cloned();
        let scroll = self.list.scroll_offset();
        let keep_folded = props.expanded.is_none();
        let keep_selected = props.selection.is_none();
        let key = self.key.clone();
        *self = Self::build(&key, props);
        if keep_folded {
            self.marks.restore_folded(folded);
        }
        if keep_selected {
            let mut chosen = KeySelection::new();
            for key in selected {
                if self.nodes.index.contains_key(&key) {
                    chosen.toggle(key);
                }
            }
            *self.list.selection_mut() = chosen;
        }
        if let Some(key) = focused {
            if self.nodes.index.contains_key(&key) {
                self.list.focus_row(key);
            }
        }
        if scroll > 0.0 {
            self.list.set_scroll_offset(scroll);
        }
        self.list
            .set_feel(motion.unwrap_or(crate::motion::Feel::Standard));
        true
    }
}

/// A tree view is its list.
impl Decorator for DescribedTreeView {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        &self.list
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        &mut self.list
    }

    fn name(&self) -> &'static str {
        "TreeView"
    }

    /// The list's change is the described tree's: it adds no structure of its own.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.list.take_structure_change()
    }
}

/// A branch "Root" over a leaf "Child", as a described component: the rows a headless run
/// acts on. It reports the row a person chooses, so a headless run reads the choice it makes.
#[cfg(test)]
pub(crate) fn items_tree(id: &str, name: Option<&str>, open: bool) -> DescribedTreeView {
    let node = |label: &str, item: &str, parent: Option<&str>, branch: bool| TreeViewNodeProps {
        item: ResourceId(item.into()),
        label: label.into(),
        parent: parent.map(|parent| ResourceId(parent.into())),
        branch,
        ..TreeViewNodeProps::default()
    };
    DescribedTreeView::from_props(
        id,
        TreeViewProps {
            name: name.map(str::to_owned),
            hide_root: true,
            nodes: vec![
                node("Root", "1", None, true),
                node("Child", "2", Some("1"), false),
            ],
            expanded: Some(if open {
                vec![ResourceId("1".into())]
            } else {
                Vec::new()
            }),
            ..TreeViewProps::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Fired, Intent};

    fn node(
        key: &str,
        parent: Option<&str>,
        branch: bool,
        queue: &crate::Intents<Fired>,
    ) -> TreeViewNodeProps {
        TreeViewNodeProps {
            item: ResourceId(key.to_owned()),
            label: key.to_owned(),
            parent: parent.map(|parent| ResourceId(parent.to_owned())),
            branch,
            drop: Event::View(Intent::new(queue, key, "drop")),
            into: Event::View(Intent::new(queue, key, "into")),
            check: Event::View(Intent::new(queue, key, "check")),
            ..TreeViewNodeProps::default()
        }
    }

    fn tree_of(
        nodes: Vec<TreeViewNodeProps>,
        hide_root: bool,
        root_drop: Event,
    ) -> DescribedTreeView {
        DescribedTreeView::from_props(
            "tree",
            TreeViewProps {
                hide_root,
                nodes,
                drop: root_drop,
                ..TreeViewProps::default()
            },
        )
    }

    /// Two people moving a node into each other's subtree at the same moment can leave links
    /// that close a loop. Every node is still held once: the one that comes last in the order
    /// does not follow its link and stands at the top level.
    #[test]
    fn parents_that_close_a_loop_keep_every_node_once() {
        let queue = crate::Intents::default();
        let tree = tree_of(
            vec![
                node("a", Some("b"), false, &queue),
                node("b", Some("a"), true, &queue),
                node("c", Some("a"), false, &queue),
                node("itself", Some("itself"), false, &queue),
            ],
            true,
            Event::None,
        );
        let nodes = tree.nodes();
        assert_eq!(nodes.len(), 4, "every node is held once");
        let parent = |key: &str| {
            let at = nodes.index[key];
            nodes.tree_at(at).and_then(|row| row.parent)
        };
        assert_eq!(parent("b"), None);
        assert_eq!(parent("a"), Some("b".to_owned()));
        assert_eq!(parent("c"), Some("a".to_owned()));
        assert_eq!(parent("itself"), None);
        assert_eq!(nodes.children[TOP_LEVEL], ["b", "itself"]);
    }

    /// Hiding the root drops the tree's own name and keeps every node at the top level, one of
    /// them as much as several: the heading above the tree names it once.
    #[test]
    fn hiding_the_root_keeps_every_top_level_node() {
        let queue = crate::Intents::default();
        let named = DescribedTreeView::from_props(
            "tree",
            TreeViewProps {
                name: Some("Subtasks".to_owned()),
                nodes: vec![
                    node("a", None, false, &queue),
                    node("b", None, false, &queue),
                ],
                ..TreeViewProps::default()
            },
        );
        let labels: Vec<(String, Option<String>)> = (0..named.nodes().len())
            .map(|at| {
                let node = &named.nodes().nodes[at];
                (node.label.clone(), node.parent.clone())
            })
            .collect();
        assert_eq!(
            labels,
            [
                ("Subtasks".to_owned(), None),
                ("a".to_owned(), Some(TOP_LEVEL.to_owned())),
                ("b".to_owned(), Some(TOP_LEVEL.to_owned())),
            ],
            "the tree's name stands over its nodes when it is not hidden"
        );
        let hidden = DescribedTreeView::from_props(
            "tree",
            TreeViewProps {
                name: Some("Subtasks".to_owned()),
                hide_root: true,
                nodes: vec![
                    node("a", None, false, &queue),
                    node("b", None, false, &queue),
                ],
                ..TreeViewProps::default()
            },
        );
        let labels: Vec<(String, Option<String>)> = (0..hidden.nodes().len())
            .map(|at| {
                let node = &hidden.nodes().nodes[at];
                (node.label.clone(), node.parent.clone())
            })
            .collect();
        assert_eq!(
            labels,
            [("a".to_owned(), None), ("b".to_owned(), None),],
            "the node's rows are the nodes, not the tree's name"
        );
    }

    /// A move is reported by the node it lands at: the next sibling it goes before, the node
    /// it goes inside of, or the tree's own end.
    #[test]
    fn a_move_is_reported_by_the_node_it_lands_at() {
        let queue = crate::Intents::default();
        let tree = tree_of(
            vec![
                node("a", None, true, &queue),
                node("b", Some("a"), false, &queue),
                node("c", None, false, &queue),
            ],
            true,
            Event::View(Intent::new(&queue, "tree", "drop")),
        );
        let nodes = tree.nodes();
        let moved = |key: &str| Some(Value::Resource(key.to_owned()));

        assert!(nodes.report(&ReorderDrop {
            source: "c".to_owned(),
            target: DropTarget::new("b".to_owned(), DropPlacement::Before),
        }));
        assert_eq!(
            queue.take(),
            [Fired {
                key: "b".into(),
                event: "drop".into(),
                value: moved("c"),
            }]
        );

        // Inside a branch: it lands before the branch's first child, so that child's node is
        // told, and the owner puts it under the same parent the child has.
        assert!(nodes.report(&ReorderDrop {
            source: "c".to_owned(),
            target: DropTarget::new("a".to_owned(), DropPlacement::Inside),
        }));
        assert_eq!(
            queue.take(),
            [Fired {
                key: "b".into(),
                event: "drop".into(),
                value: moved("c"),
            }]
        );

        // Inside a node with nothing in it: the node itself is told it took it.
        assert!(nodes.report(&ReorderDrop {
            source: "c".to_owned(),
            target: DropTarget::new("c".to_owned(), DropPlacement::Inside),
        }));
        assert_eq!(
            queue.take(),
            [Fired {
                key: "c".into(),
                event: "into".into(),
                value: moved("c"),
            }]
        );

        // Past every row: the end of the top level.
        assert!(nodes.report(&ReorderDrop {
            source: "b".to_owned(),
            target: DropTarget::new(String::new(), DropPlacement::After),
        }));
        assert_eq!(
            queue.take(),
            [Fired {
                key: "tree".into(),
                event: "drop".into(),
                value: moved("b"),
            }]
        );
    }

    /// How far a branch got is the owner's count, said of the node: the row shows it, and a
    /// row the app worded itself keeps its own. Open, its own rows say it.
    #[test]
    fn a_node_says_how_far_its_branch_got() {
        let queue = crate::Intents::default();
        let mut said = node("a", None, true, &queue);
        said.progress = Some("1/3".into());
        said.row = Some(TreeViewRowProps::default());
        // Its children say nothing of a count: the node is asked, not counted.
        let mut child = node("b", Some("a"), false, &queue);
        child.row = Some(TreeViewRowProps {
            checkbox: Some(true),
            ..TreeViewRowProps::default()
        });
        let mut worded = node("c", Some("a"), false, &queue);
        worded.progress = Some("1/3".into());
        worded.row = Some(TreeViewRowProps {
            progress: Some("2/5".into()),
            ..TreeViewRowProps::default()
        });
        let tree = tree_of(vec![said, child, worded], true, Event::None);
        let nodes = tree.nodes();
        let of = |key: &str| progress_of(nodes.node(key).expect("a node"), true);
        assert_eq!(of("a").as_deref(), Some("1/3"));
        assert_eq!(of("b"), None, "nothing says it");
        assert_eq!(
            of("c").as_deref(),
            Some("2/5"),
            "the row's own wording stands"
        );
        assert_eq!(
            progress_of(nodes.node("a").expect("a node"), false),
            None,
            "open, its own rows say how far it got"
        );
        assert_eq!(
            progress_of(nodes.node("b").expect("a node"), false),
            None,
            "a leaf says nothing of a count"
        );
    }

    /// A folder says it is a branch before its files are known: its row shows the disclosure
    /// that opens it, even when no node under it is in the list yet.
    #[test]
    fn a_folder_with_nothing_in_it_still_shows_its_disclosure() {
        let queue = crate::Intents::default();
        let tree = tree_of(
            vec![
                node("folder", None, true, &queue),
                node("file", None, false, &queue),
            ],
            true,
            Event::None,
        );
        let nodes = tree.nodes();
        let of = |key: &str| nodes.index[key];
        assert!(
            nodes.has_subtasks(of("folder")),
            "a branch shows its disclosure with nothing under it"
        );
        assert!(!nodes.has_subtasks(of("file")), "a leaf folds nothing");
        assert_eq!(
            nodes.folded_at_first(&[]),
            vec!["folder".to_owned()],
            "a view that holds nothing open folds its branch, and the list opens it when read"
        );
    }

    /// A press on a tree's row chooses it whether or not the view names a choice event: the
    /// tree keeps the row under the keyboard and the pointer alike, as every list does.
    #[test]
    fn a_press_chooses_a_trees_row_whatever_the_view_reports() {
        use crate::headless::FrameHarness;
        use crate::theme::Theme;

        let queue = crate::Intents::default();
        for select in [
            Event::None,
            Event::View(Intent::new(&crate::Intents::default(), "tree", "select")),
        ] {
            let tree = tree_of(
                vec![
                    node("a", None, true, &queue),
                    node("b", Some("a"), false, &queue),
                ],
                true,
                select,
            );
            let mut harness =
                FrameHarness::for_root(crate::State::new(), tree, (400, 240), Theme::dark());
            let row = harness
                .root::<DescribedTreeView>()
                .list
                .row_rect(&"b".to_owned())
                .expect("the leaf row stands somewhere");
            harness.click_at(row.center().x, row.center().y);
            assert_eq!(
                harness.root::<DescribedTreeView>().selected(),
                ["b".to_owned()],
                "a press chooses the row"
            );
        }
    }

    /// A row with something to fold keeps everything a leaf row keeps: its box and its words
    /// stand on the column of its depth, and the chevron that folds it stands in its own step
    /// in front of them, clear of the box.
    #[test]
    fn a_branch_row_keeps_its_box_and_words_on_its_depths_column() {
        use crate::headless::{FrameHarness, InspectionNode};
        use crate::theme::Theme;
        use crate::widget::TREE_INDENT;
        use crate::widget::{LayoutInfo, RowLayout};

        let queue = crate::Intents::default();
        let boxed = |key: &str, parent: Option<&str>, branch: bool, checked: bool| {
            let mut props = node(key, parent, branch, &queue);
            props.row = Some(TreeViewRowProps {
                checkbox: Some(checked),
                ..TreeViewRowProps::default()
            });
            props
        };
        let tree = tree_of(
            vec![
                boxed("Find the last service date", None, true, false),
                boxed("Ask whether Tuesday works", None, false, true),
                boxed(
                    "Look up the boiler's year",
                    Some("Find the last service date"),
                    false,
                    false,
                ),
            ],
            true,
            Event::None,
        );
        let harness = FrameHarness::for_root(crate::State::new(), tree, (400, 240), Theme::dark());
        let snapshot = harness.inspection();
        fn rows(node: &InspectionNode, out: &mut Vec<(String, RowLayout)>) {
            if let Some(LayoutInfo::Row(row)) = &node.layout {
                let name = node
                    .semantics
                    .as_ref()
                    .and_then(|access| access.name.clone())
                    .unwrap_or_default();
                out.push((name, *row));
            }
            for child in &node.children {
                rows(child, out);
            }
        }
        let mut laid = Vec::new();
        rows(&snapshot.root, &mut laid);
        let of = |words: &str| {
            laid.iter()
                .find(|(name, _)| name == words)
                .map(|(_, row)| *row)
                .unwrap_or_else(|| panic!("the row {words:?} stands in the tree"))
        };
        let branch = of("Find the last service date");
        let leaf = of("Ask whether Tuesday works");
        assert_eq!(branch.depth, 0, "the branch stands at the top");
        assert_eq!(leaf.depth, 0, "so does the leaf");
        assert_eq!(
            branch.content_left, leaf.content_left,
            "a branch keeps its box on the column a leaf's stands on: {} against {}",
            branch.content_left, leaf.content_left
        );
        assert_eq!(
            branch.text_x, leaf.text_x,
            "and its words: {} against {}",
            branch.text_x, leaf.text_x
        );

        // The chevron stands in the step the row keeps free in front of its box, so it is
        // drawn beside the box rather than over it.
        let chevron = harness
            .root::<DescribedTreeView>()
            .list
            .disclosure_rect(&"Find the last service date".to_owned())
            .expect("the branch shows its disclosure");
        assert!(
            chevron.x1 <= f64::from(branch.content_left) + 0.5,
            "the chevron ends before the box begins: {} against {}",
            chevron.x1,
            branch.content_left
        );
        assert!(
            chevron.x0 >= f64::from(branch.content_left) - f64::from(TREE_INDENT) - 0.5,
            "and it hangs inside the step in front of it: {} against {}",
            chevron.x0,
            branch.content_left
        );
    }
}
