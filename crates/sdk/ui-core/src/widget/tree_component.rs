//! `TreeView` as a described component: a tree of nodes, each a part in a slot, with its
//! children named under it by key, and selecting or activating a node reported with the
//! resource it stands for.
//!
//! The nodes are [`TreeViewNodeProps`], one per row, each naming its parent by the resource it
//! stands for, each taking the change mark of what someone else changed of it
//! ([`TreeViewNodeProps::change`]), and the rows are drawn by the list's own tree
//! ([`VirtualList::with_tree`](crate::widget::VirtualList::with_tree)): a node that changed is
//! one part beside the others, and the tree changes in place — an expanded branch folds where
//! it stands, and the selection follows what the view says. Folding, keyboard navigation,
//! focus, type-ahead, the row's menu and its drag all come from the list, so a tree here
//! behaves as any other list of rows does.
//!
//! The rows are virtual: a host renders them as the list shows them
//! ([`crate::component::RowSource`]), so a notebook of ten thousand notes costs the rows on
//! screen. What the list needs of a row it does not hold — whether it opens, how deep it
//! stands — a row says itself ([`TreeViewNodeProps::branch`], [`TreeViewNodeProps::depth`]), so
//! the window it shows needs no row outside it.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::rc::Rc;

use crate::component::{
    Accepts, Component, Described, Event, Person, ResourceId, SharedRows, SlotType, Source, Value,
};
use crate::icons::Icon;
use crate::reorder::{DropPlacement, DropTarget, ReorderDrop};
use crate::selection::KeySelection;
use crate::widget::nav_list::NavigationSource;
use crate::widget::{
    Button, ButtonProps, ChangeMarkProps, Decorator, Element, HoverToolbar, ListModel, ListRow,
    MenuItem, MenuItemProps, Press, TreeMarks, TreeRow, VirtualList, Widget,
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
    /// Who else is at the item now (a collaborator's or an agent's lens on it), shown on the
    /// row as a list row shows it ([`crate::widget::ListItemProps::presence`]).
    pub presence: Option<Person>,
    /// The item was removed and the person has not seen it go: the row stands in for it, its
    /// title struck through, at the row's own height. The UI host places such rows.
    pub removed: bool,
    /// How deep it stands, when the view says it: 0 at the top level. Given, the tree indents
    /// the row by it rather than by the rows over it, so a row whose parent stands off screen
    /// keeps its place, and a tree whose rows the host renders as it shows them indents rows it
    /// never built.
    pub depth: Option<i64>,
    /// The node the view shows just over this one under the same parent: the place the actions
    /// offered on it take when one reads a place (`above`). None for a first child.
    pub above: Option<ResourceId>,
    /// Whether it opens to show the nodes under it. A node with children says so unless it
    /// says otherwise.
    pub branch: bool,
    /// Whether activating the row opens the item it stands for in the window's own page, as a
    /// list's row does. `Some(false)` where the window draws the open item itself — a tree
    /// beside its page — and answers the activation: the row reports its events and opens
    /// nothing of the host's own, so the page the tree shows is the only one.
    pub navigates: Option<bool>,
    /// What the row shows besides its label.
    #[component(slot)]
    pub row: Option<TreeViewRowProps>,
    /// How far the branch got ("1/3"), which the owner counts of its own children: the same
    /// count names a list row and an outline node, so both say one thing, folded or open.
    pub progress: Option<String>,
    /// Whether it is the selected node.
    pub selected: bool,
    /// This node's checkbox was pressed: the owner toggles what the box stands for.
    #[component(event)]
    pub check: Event,
    /// The commands of its context menu: a right-click, the Menu key or a long press opens it,
    /// so every action a row has stays reachable without a drag.
    #[component(slot)]
    pub menu: Vec<MenuItemProps>,
    /// The controls the row shows over its trailing corner while it is hovered or holds focus:
    /// the quick action of the thing it stands for (a note's "+", a folder's), without opening a
    /// menu. A node that gives none shows no hover toolbar.
    #[component(slot)]
    pub toolbar: Vec<ButtonProps>,
    /// What someone else changed of the row's resource that the person has not seen yet: the
    /// row shows it, in their colour, on its trailing edge. The UI host places it
    /// (task-legibility §3).
    #[component(slot)]
    pub change: Option<ChangeMarkProps>,
}

crate::component::part_slot!(TreeViewNodeProps);
crate::component::part_slot!(TreeViewRowProps);
crate::component::part_slot!(ButtonProps);

impl TreeViewRows {
    /// The nodes the view gave whole, as it wrote them.
    #[must_use]
    pub fn given(nodes: Vec<TreeViewNodeProps>) -> Self {
        Self {
            nodes: Held::Given(nodes),
        }
    }

    /// How many nodes the tree holds, rendered or not: what a host says it renders of them all.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.nodes {
            Held::Nothing => 0,
            Held::Given(nodes) => nodes.len(),
            Held::Rendered(rows) => rows.try_borrow().map_or(0, |rows| rows.len()),
        }
    }

    /// Whether the tree holds no node.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl From<Vec<TreeViewNodeProps>> for TreeViewRows {
    fn from(nodes: Vec<TreeViewNodeProps>) -> Self {
        Self::given(nodes)
    }
}

/// The nodes of a tree: the ones the view gives whole, or the rows a host renders as the list
/// shows them. Either way the list reads them one at a time, as it mounts the rows it shows.
#[derive(Default)]
pub struct TreeViewRows {
    nodes: Held,
}

/// Where the nodes of a tree come from.
#[derive(Default)]
enum Held {
    /// None: an empty tree.
    #[default]
    Nothing,
    /// The nodes the view gave whole, as it wrote them.
    Given(Vec<TreeViewNodeProps>),
    /// The nodes a host renders as the list shows them, one row at a time.
    Rendered(SharedRows),
}

impl SlotType for TreeViewRows {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(TreeViewNodeProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        if let Some(rows) = source.rows(slot) {
            self.nodes = Held::Rendered(rows);
            return Ok(());
        }
        let mut given = Vec::new();
        source.components(slot, &mut |child| {
            given.push(TreeViewNodeProps::from_source(child).map_err(|error| error.to_string())?);
            Ok(())
        })?;
        self.nodes = Held::Given(given);
        Ok(())
    }
}

/// The rows of a tree that stand in for nodes removed unseen
/// ([`TreeViewNodeProps::removed`]), in order.
#[derive(Default)]
pub struct RemovedNodes(Vec<TreeViewNodeProps>);

impl From<Vec<TreeViewNodeProps>> for RemovedNodes {
    fn from(nodes: Vec<TreeViewNodeProps>) -> Self {
        Self(nodes)
    }
}

impl SlotType for RemovedNodes {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(TreeViewNodeProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let mut nodes: Vec<TreeViewNodeProps> = Vec::new();
        source.components(slot, &mut |child| {
            let node = TreeViewNodeProps::from_source(child).map_err(|error| error.to_string())?;
            if !node.removed {
                return Err("the removed rows of a tree stand for removed nodes".to_owned());
            }
            nodes.push(node);
            Ok(())
        })?;
        self.0 = nodes;
        Ok(())
    }
}

/// A tree of nodes: a file explorer, a project's files, the layers of a drawing, a book's
/// pages. Each node is the resource it stands for; opening a branch shows the nodes under it,
/// and selecting or activating a node reports its resource.
#[derive(Component, Default)]
#[component(name = "TreeView", role = "Tree", label = "name")]
pub struct TreeViewProps {
    /// What the tree is, for assistive technology.
    pub name: Option<String>,
    /// The tree takes the height it is given and scrolls inside itself, rather than sizing to
    /// the rows inside another scroll. Such a tree is the pane's list, so its rows stand on the
    /// pane's content edge (where the pane's header starts its title) with their lift bleeding
    /// past it, as every list in a column does — including a phone's window, where the pane is
    /// the whole screen and the rows keep the page gutter at its glass.
    pub fill: bool,
    /// The tree's root node — the tree itself, labelled with its `name` — is not a row: the nodes at
    /// the top level are, at the first depth. Off, the tree's name stands over them as a row of its
    /// own.
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
    pub nodes: TreeViewRows,
    /// The rows standing in for nodes someone else removed that the person has not seen go:
    /// they stand after the nodes, struck, each with its mark. The UI host places them.
    #[component(slot)]
    pub removed: RemovedNodes,
    /// The nodes open now, by the resource each stands for. Given, it is where the view says the tree
    /// is; without it the tree keeps what a person opened.
    #[component(state)]
    pub expanded: Option<Vec<ResourceId>>,
    /// The rows a search found, by the resource each stands for, when the tree shows what the
    /// search narrowed it to: the rows the tree shows that are not among them stand for where
    /// the found ones are, and read in the quietest ink. None, every row is one the tree shows
    /// for itself.
    #[component(state)]
    pub matching: Option<Vec<ResourceId>>,
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
    /// A row was dropped just before another, or moved there by the keyboard: carries the row
    /// that moved and the row it lands before — none for the end of the top level.
    #[component(event(value = ResourceId, at = Option<ResourceId>))]
    pub drop: Event,
    /// A row was dropped inside another, at the end of its own, or indented there by the
    /// keyboard: carries the row that moved and the row it lands inside — none for the end of
    /// the top level, which stands inside no row.
    #[component(event(value = ResourceId, at = Option<ResourceId>))]
    pub into: Event,
    /// A row was dropped just after another and all that stands under it, or moved there by the
    /// keyboard: carries the row that moved and the row it lands after. The view places it past
    /// that row's own, which only the view can name: what stands under them are rows the tree
    /// may not have built.
    #[component(event(value = ResourceId, at = ResourceId))]
    pub after: Event,
}

/// A described tree: its list, the rows it was built from, and what it keeps of them.
pub struct DescribedTreeView {
    /// The widget id the tree was made with, kept so a change builds it again under the same
    /// id.
    key: String,
    list: VirtualList<TreeRows>,
    /// Where the rows are: the nodes the view gave whole, or the rows a host renders as the
    /// list shows them.
    rows: TreeRows,
    /// What the list works out of the rows as it mounts them, and what the person folded.
    marks: Rc<TreeMarks<String>>,
    /// What the person had folded before the view said where the tree is (`expanded`), while it
    /// says it: the folding their own, which comes back when the view stops saying it.
    own_folds: Option<Vec<String>>,
    /// The person opened a row of the tree since the window hosting it last asked: what a window
    /// showing one column at a time moves on from, as a list of places tells it.
    navigated: Rc<Cell<bool>>,
}

/// The key of the top level: the row of the tree's own name, over the nodes without a parent.
const TOP_LEVEL: &str = "";

/// What a tree reports of a move: the row that moved and the row it lands at, by the resource
/// each shows. The tree's own, so a move lands whether or not the row it lands at was built.
#[derive(Clone, Default)]
struct Moves {
    /// A row lands just before another row, or at the end of the top level (none).
    drop: Event,
    /// A row lands inside another, at the end of its own.
    into: Event,
    /// A row lands just after another and all that stands under it.
    after: Event,
}

/// One row of a tree, its parts read once.
#[derive(Clone)]
struct Node {
    key: String,
    /// The resource it stands for, which its events carry.
    item: ResourceId,
    label: String,
    /// The row it stands under, as the list sees it: the tree's own row over a top-level node,
    /// or none where the tree hides that row.
    parent: Option<String>,
    /// How deep it stands, when the view says it: the list indents by it rather than by the
    /// rows over it.
    depth: Option<u32>,
    /// Whether it opens to show nodes under it, whether or not any of them stands in the list.
    branch: bool,
    /// Who else is at the item now, shown on the row.
    presence: Option<Person>,
    /// It stands in for an item someone removed, its title struck through.
    removed: bool,
    row: Option<TreeViewRowProps>,
    progress: Option<String>,
    menu: Vec<Command>,
    toolbar: Vec<ButtonProps>,
    change: Option<Rc<ChangeMarkProps>>,
    /// The row the view says stands just over this one under the same parent: the row `Tab`
    /// indents it under and `Alt-Up` goes before, held or not.
    above: Option<String>,
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
    /// The rows that open to show subtasks.
    branches: HashSet<String>,
    /// The key each resource stands at.
    keys: HashMap<String, String>,
    /// What the tree reports of a move: the row that moved, and the row it lands at.
    moves: Moves,
    /// Whether any node offers a menu, which the list offers of its rows only then.
    has_menu: bool,
}

/// One node of a tree, its parts read once.
struct Part {
    key: String,
    item: ResourceId,
    label: String,
    parent: Option<String>,
    depth: Option<u32>,
    branch: bool,
    presence: Option<Person>,
    removed: bool,
    row: Option<TreeViewRowProps>,
    progress: Option<String>,
    menu: Vec<MenuItemProps>,
    toolbar: Vec<ButtonProps>,
    change: Option<ChangeMarkProps>,
    /// The row the view says stands just over this one under the same parent.
    above: Option<ResourceId>,
    check: Event,
}

/// One command of a node's context menu, kept so the menu can be offered each time it opens.
#[derive(Clone)]
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

/// One node as the view wrote it, its parts kept as the list reads them.
fn part_of(node: TreeViewNodeProps) -> Part {
    Part {
        key: node.item.0.clone(),
        item: node.item,
        label: node.label,
        parent: node.parent.map(|parent| parent.0),
        depth: node.depth.and_then(|depth| u32::try_from(depth).ok()),
        branch: node.branch,
        presence: node.presence,
        removed: node.removed,
        row: node.row,
        progress: node.progress,
        menu: node.menu,
        toolbar: node.toolbar,
        change: node.change,
        above: node.above,
        check: node.check,
    }
}

impl Nodes {
    /// The rows of `props`: the tree's own name over the nodes when it shows one, then every
    /// node, each naming the row it stands under.
    fn of(
        props: Vec<TreeViewNodeProps>,
        hide_root: bool,
        name: Option<String>,
        moves: Moves,
    ) -> Self {
        let mut parts = Vec::with_capacity(props.len());
        for node in props {
            parts.push(part_of(node));
        }
        let keys: HashSet<String> = parts.iter().map(|part| part.key.clone()).collect();
        let has_menu = parts.iter().any(|part| !part.menu.is_empty());
        let root_shown = !hide_root;
        let mut nodes: Vec<Node> = Vec::with_capacity(parts.len() + 1);
        let mut index = HashMap::new();
        let mut children: HashMap<String, Vec<String>> = HashMap::new();
        let mut branches = HashSet::new();
        let mut item_keys = HashMap::new();
        if root_shown {
            index.insert(TOP_LEVEL.to_owned(), 0);
            children.entry(TOP_LEVEL.to_owned()).or_default();
            branches.insert(TOP_LEVEL.to_owned());
            nodes.push(Node {
                key: TOP_LEVEL.to_owned(),
                item: ResourceId(TOP_LEVEL.to_owned()),
                label: name.unwrap_or_default(),
                parent: None,
                depth: Some(0),
                branch: true,
                presence: None,
                removed: false,
                row: None,
                progress: None,
                menu: Vec::new(),
                toolbar: Vec::new(),
                change: None,
                above: None,
                check: Event::None,
            });
        }
        let mut parents: HashMap<String, Option<String>> = HashMap::new();
        for part in parts {
            if index.contains_key(&part.key) {
                continue;
            }
            let parent = match part.parent {
                Some(parent) if keys.contains(&parent) && parent != part.key => Some(parent),
                _ => None,
            };
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
                branches.insert(part.key.clone());
            }
            let under = parent.clone().unwrap_or_else(|| TOP_LEVEL.to_owned());
            children.entry(under).or_default().push(part.key.clone());
            index.insert(part.key.clone(), nodes.len());
            nodes.push(Node {
                key: part.key.clone(),
                item: part.item.clone(),
                label: part.label,
                parent: if root_shown {
                    Some(parent.unwrap_or_else(|| TOP_LEVEL.to_owned()))
                } else {
                    parent
                },
                depth: part.depth,
                branch: part.branch,
                presence: part.presence,
                removed: part.removed,
                row: part.row,
                progress: part.progress,
                menu: part.menu.into_iter().map(Command::of).collect(),
                toolbar: part.toolbar,
                change: part.change.map(Rc::new),
                above: part
                    .above
                    .as_ref()
                    .filter(|above| keys.contains(&above.0))
                    .map(|above| above.0.clone()),
                check: part.check,
            });
            item_keys.insert(part.item.0.clone(), part.key.clone());
        }
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
        for (key, branch) in with_children.iter().map(|key| (key.clone(), true)) {
            branches.insert(key.clone());
            if let Some(node) = index.get(&key).and_then(|at| nodes.get_mut(*at)) {
                node.branch |= branch;
            }
        }
        Self {
            inner: Rc::new(Inner {
                nodes,
                index,
                branches,
                keys: item_keys,
                moves,
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

    /// The resource the node at `key` stands for.
    #[cfg(test)]
    fn resource_of(&self, key: &str) -> Option<&ResourceId> {
        self.node(key).map(|node| &node.item)
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
        let node = self.nodes.get(index)?;
        Some(TreeRow {
            parent: node.parent.clone(),
            depth: node.depth,
            above: node.above.clone(),
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

/// The rows of a tree, as its list reads them: the nodes the view gave whole, or the rows a
/// host renders as the list shows them, and after them the rows standing in for nodes someone
/// else removed that the person has not seen go.
#[derive(Clone)]
struct TreeRows {
    rows: Rows,
    /// The struck rows of removed nodes, which the host gives whole: they stand after the rows,
    /// at the top level.
    removed: Rc<Vec<Node>>,
}

/// Where a tree's rows come from.
#[derive(Clone)]
enum Rows {
    /// The nodes the view gave whole, read once with what the list asks of them.
    Given(Nodes),
    /// The nodes a host renders as the list shows them, read as the list mounts each row.
    Rendered(Rc<Remote>),
}

/// The rows of a tree a host renders as the list shows them: a notebook of ten thousand notes
/// costs the rows on screen, and none stands in the list until it is shown.
struct Remote {
    source: SharedRows,
    /// The rows read so far, by key: a row the host has not rendered reads as none.
    read: RefCell<HashMap<String, Option<Rc<Node>>>>,
    /// The resource each unbuilt row stands for, by key, as the rows that named it said: a
    /// row's parent and the row over it, which a move lands at and a view hears of without the
    /// row being on screen.
    named: RefCell<HashMap<String, ResourceId>>,
    /// What the list works out of the rows, for the rows the view says stand closed.
    marks: RefCell<Option<Rc<TreeMarks<String>>>>,
    /// The rows the view says it holds open, when it says which: a row read as a branch and not
    /// among them folds what stands under it, as the rows a view gives whole do.
    open: Option<Rc<HashSet<String>>>,
    /// What the tree reports of a move.
    moves: Moves,
    /// Whether any row offers a menu, which the list offers of its rows only then.
    menu: std::cell::Cell<bool>,
}

impl Remote {
    /// The row `key`, read from its host when it is rendered, and kept.
    fn node(&self, key: &str) -> Option<Rc<Node>> {
        if let Some(read) = self.read.borrow().get(key) {
            return read.clone();
        }
        let read = self.read_row(key);
        if let Some(node) = &read {
            if !node.menu.is_empty() {
                self.menu.set(true);
            }
            self.fold_if_told(node);
        }
        self.read.borrow_mut().insert(key.to_owned(), read.clone());
        read
    }

    /// The row `key` as its host renders it, or none while it is not rendered.
    fn read_row(&self, key: &str) -> Option<Rc<Node>> {
        let mut read: Option<Part> = None;
        let mut take = |source: &mut dyn Source| {
            let props =
                TreeViewNodeProps::from_source(source).map_err(|error| error.to_string())?;
            read = Some(part_of(props));
            Ok(())
        };
        let shown = self
            .source
            .try_borrow_mut()
            .ok()?
            .read(key, &mut take)
            .ok()?;
        let part = shown.then_some(read).flatten()?;
        let mut keyed = |id: &str| {
            let at = self.parent_key(id)?;
            self.named
                .borrow_mut()
                .insert(at.clone(), ResourceId(id.to_owned()));
            Some(at)
        };
        let parent = part.parent.as_deref().and_then(&mut keyed);
        let above = part.above.as_ref().and_then(|above| keyed(&above.0));
        Some(Rc::new(Node {
            key: key.to_owned(),
            item: part.item,
            label: part.label,
            parent,
            depth: part.depth,
            branch: part.branch,
            presence: part.presence,
            removed: part.removed,
            row: part.row,
            progress: part.progress,
            menu: part.menu.into_iter().map(Command::of).collect(),
            toolbar: part.toolbar,
            change: part.change.map(Rc::new),
            above,
            check: part.check,
        }))
    }

    /// The key of the row showing the resource `parent`, when the list holds one.
    fn parent_key(&self, parent: &str) -> Option<String> {
        self.source.try_borrow().ok()?.key_presenting(parent)
    }

    /// Fold what stands under a row the view says stands closed, once it is read: the view's
    /// word is the tree's, and the person's folding stands after it.
    fn fold_if_told(&self, node: &Node) {
        let Some(open) = &self.open else {
            return;
        };
        let marks = self.marks.borrow();
        let Some(marks) = marks.as_ref() else {
            return;
        };
        if node.branch && !open.contains(&node.key) {
            marks.fold(&node.key, true);
        }
    }

    /// The rows read so far changed: those named are read again as the list mounts them.
    fn forget(&self, keys: &[String]) {
        let mut read = self.read.borrow_mut();
        for key in keys {
            read.remove(key);
        }
    }

    /// The resource the row `key` stands for, for a row not read: what the rows that named it
    /// said of it.
    fn named(&self, key: &str) -> Option<Value> {
        self.named
            .borrow()
            .get(key)
            .map(|item| Value::Resource(item.0.clone()))
    }

    /// Every row is read again.
    fn clear(&self) {
        self.read.borrow_mut().clear();
        self.named.borrow_mut().clear();
        self.menu.set(true);
    }
}

/// A row of a tree, held where the tree gave it or read from the host for this look.
enum NodeRef<'a> {
    Given(&'a Node),
    Rendered(Rc<Node>),
}

impl Deref for NodeRef<'_> {
    type Target = Node;

    fn deref(&self) -> &Node {
        match self {
            Self::Given(node) => node,
            Self::Rendered(node) => node,
        }
    }
}

impl TreeRows {
    /// The rows of `nodes`: the tree's own name over them when it shows one, or the rows the
    /// host renders as the list shows them, with the rows the view says stand open — `expanded`,
    /// mapped to the rows of the items it names by `keyed` — and the struck rows of `removed`
    /// nodes after them.
    fn of(
        nodes: TreeViewRows,
        removed: RemovedNodes,
        hide_root: bool,
        name: Option<String>,
        expanded: Option<&[ResourceId]>,
        moves: Moves,
        marks: &Rc<TreeMarks<String>>,
    ) -> Self {
        let given = |props: Vec<TreeViewNodeProps>, moves: Moves| {
            let nodes = Nodes::of(props, hide_root, name.clone(), moves);
            if let Some(expanded) = expanded {
                marks.restore_folded(nodes.folded_at_first(expanded));
            }
            Rows::Given(nodes)
        };
        let rows = match nodes.nodes {
            Held::Nothing => given(Vec::new(), moves.clone()),
            Held::Given(props) => given(props, moves.clone()),
            Held::Rendered(source) => Rows::Rendered(Rc::new(Remote {
                source,
                read: RefCell::default(),
                named: RefCell::default(),
                marks: RefCell::new(Some(Rc::clone(marks))),
                open: expanded.map(|expanded| {
                    Rc::new(
                        expanded
                            .iter()
                            .map(|id| id.0.clone())
                            .collect::<HashSet<String>>(),
                    )
                }),
                moves,
                menu: std::cell::Cell::new(true),
            })),
        };
        // // A struck row stands at the top level, after every other row: the row it stood under // is one
        // the tree may never have built, and its branch may be folded away.
        let struck: Vec<Node> = removed
            .0
            .into_iter()
            .map(|props| {
                let key = format!("{}~removed", props.item.0);
                Node {
                    key,
                    item: props.item,
                    label: props.label,
                    parent: (!hide_root).then(|| TOP_LEVEL.to_owned()),
                    depth: Some(0),
                    branch: false,
                    presence: props.presence,
                    removed: true,
                    row: props.row,
                    progress: props.progress,
                    menu: props.menu.into_iter().map(Command::of).collect(),
                    toolbar: props.toolbar,
                    change: props.change.map(Rc::new),
                    above: None,
                    check: props.check,
                }
            })
            .collect();
        Self {
            rows,
            removed: Rc::new(struck),
        }
    }

    /// How many rows the tree holds before the struck rows of removed nodes.
    fn held_len(&self) -> usize {
        match &self.rows {
            Rows::Given(nodes) => nodes.len(),
            Rows::Rendered(remote) => remote.source.try_borrow().map_or(0, |rows| rows.len()),
        }
    }

    /// The struck row standing at `index`, when it is one of the rows after the nodes.
    fn removed_at(&self, index: usize) -> Option<&Node> {
        self.removed.get(index.checked_sub(self.held_len())?)
    }

    /// The row `key`, or none when the tree does not hold it.
    fn node(&self, key: &str) -> Option<NodeRef<'_>> {
        if let Some(node) = self.removed.iter().find(|node| node.key == key) {
            return Some(NodeRef::Given(node));
        }
        match &self.rows {
            Rows::Given(nodes) => nodes.node(key).map(NodeRef::Given),
            Rows::Rendered(remote) => remote.node(key).map(NodeRef::Rendered),
        }
    }

    /// The key of the row standing for the resource `id`, when the tree holds one.
    fn key_of_item(&self, id: &str) -> Option<String> {
        match &self.rows {
            Rows::Given(nodes) => nodes.key_of_item(id),
            Rows::Rendered(remote) => remote.parent_key(id),
        }
    }

    /// Whether the tree holds the row `key`.
    fn holds(&self, key: &str) -> bool {
        if self.removed.iter().any(|node| node.key == key) {
            return true;
        }
        match &self.rows {
            Rows::Given(nodes) => nodes.index.contains_key(key),
            Rows::Rendered(remote) => remote
                .source
                .try_borrow()
                .is_ok_and(|rows| rows.index_of(key).is_some()),
        }
    }

    /// The resource the row at `key` stands for, as an event carries it: read from the row when
    /// the list holds it, and from the rows that named it when it is not built.
    fn value_of(&self, key: &str) -> Option<Value> {
        self.node(key)
            .map(|node| Value::Resource(node.item.0.clone()))
            .or_else(|| match &self.rows {
                Rows::Rendered(remote) => remote.named(key),
                Rows::Given(_) => None,
            })
    }

    /// Whether any row offers a menu, which the list offers of its rows only then.
    fn has_menu(&self) -> bool {
        match &self.rows {
            Rows::Given(nodes) => nodes.has_menu,
            Rows::Rendered(remote) => remote.menu.get(),
        }
    }

    /// The commands of the row's menu, as the list offers them.
    fn menu_of(&self, key: Option<&String>) -> Vec<MenuItem> {
        key.and_then(|key| self.node(key))
            .map(|node| node.menu.iter().map(Command::entry).collect())
            .unwrap_or_default()
    }

    /// The row a node stands under: none at the top level.
    fn declared_parent(&self, key: &str) -> Option<String> {
        self.node(key)
            .and_then(|node| node.parent.clone())
            .filter(|parent| parent != TOP_LEVEL)
    }

    /// Whether `source` may land at `target`: not on itself, not inside its own subtree, and
    /// inside only a row that takes subtasks. A row the list has not built says nothing of its
    /// own, so what lands in it is the owner's to refuse.
    fn can_reorder(&self, source: &String, target: &DropTarget<String>) -> bool {
        if target.key == *source || self.value_of(source).is_none() {
            return false;
        }
        if matches!(target.placement, DropPlacement::Inside)
            && self.node(&target.key).is_some_and(|node| !node.branch)
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

    /// Where a dragged or keyed move lands, told to the tree: the row that moved and the row it lands
    /// at, by the resource each shows — none for the end of the top level. A row the list does not hold
    /// says where it lands still, from what the rows that named it said: the view places the row
    /// without the tree having built the one it lands at.
    fn report(&self, dropped: &ReorderDrop<String>) -> bool {
        let Some(moved) = self.value_of(&dropped.source) else {
            return false;
        };
        let (event, at) = if dropped.target.key == TOP_LEVEL {
            (self.drop(), None)
        } else {
            let at = self.value_of(&dropped.target.key);
            let event = match (&dropped.target.placement, &at) {
                (DropPlacement::Before, _) => self.drop(),
                (DropPlacement::Inside, Some(_)) => self.into(),
                (DropPlacement::After, Some(_)) => self.after(),
                _ => return false,
            };
            (event, at)
        };
        if event.is_none() {
            return false;
        }
        let at = at.unwrap_or(Value::None);
        event.fire(Some(Value::List(vec![moved, at])));
        true
    }

    /// The tree's own events: a row lands before another (none: the end of the top level),
    /// inside one, or after one and all that stands under it.
    fn drop(&self) -> Event {
        self.moves().drop.clone()
    }

    fn into(&self) -> Event {
        self.moves().into.clone()
    }

    fn after(&self) -> Event {
        self.moves().after.clone()
    }

    fn moves(&self) -> &Moves {
        match &self.rows {
            Rows::Given(nodes) => &nodes.moves,
            Rows::Rendered(remote) => &remote.moves,
        }
    }
}

impl ListModel for TreeRows {
    type Key = String;

    fn len(&self) -> usize {
        self.held_len() + self.removed.len()
    }

    fn key_at(&self, index: usize) -> Option<Self::Key> {
        if let Some(node) = self.removed_at(index) {
            return Some(node.key.clone());
        }
        match &self.rows {
            Rows::Given(nodes) => nodes.key_at(index),
            Rows::Rendered(remote) => remote.source.try_borrow().ok()?.key_at(index),
        }
    }

    fn index_of(&self, key: &Self::Key) -> Option<usize> {
        if let Some(at) = self.removed.iter().position(|node| node.key == *key) {
            return Some(self.held_len() + at);
        }
        match &self.rows {
            Rows::Given(nodes) => nodes.index_of(key),
            Rows::Rendered(remote) => remote.source.try_borrow().ok()?.index_of(key),
        }
    }

    fn request_range(&mut self, range: std::ops::Range<usize>, open: Option<&String>) {
        let held = self.held_len();
        let range = range.start.min(held)..range.end.min(held);
        if range.is_empty() {
            return;
        }
        if let Rows::Rendered(remote) = &self.rows
            && let Ok(mut rows) = remote.source.try_borrow_mut()
        {
            rows.request_range(range, open.cloned());
        }
    }

    fn row_state(&self, index: usize) -> crate::arriving::Arrival<(), &'static str> {
        if let Rows::Rendered(remote) = &self.rows
            && let Ok(rows) = remote.source.try_borrow()
            && let Some(key) = rows.key_at(index)
            && !rows.rendered(&key)
        {
            return crate::arriving::Arrival::local();
        }
        crate::arriving::Arrival::Ready(())
    }

    fn poll_changes(&mut self) -> Vec<crate::widget::virtual_list::ListModelChange<String>> {
        let Rows::Rendered(remote) = &self.rows else {
            return Vec::new();
        };
        let Ok(mut rows) = remote.source.try_borrow_mut() else {
            return Vec::new();
        };
        let changes = rows.take_changes();
        for change in &changes {
            match change {
                crate::widget::virtual_list::ListModelChange::RowsChanged { keys } => {
                    remote.forget(keys);
                }
                crate::widget::virtual_list::ListModelChange::Reset => remote.clear(),
                _ => {}
            }
        }
        changes
    }

    fn needs_poll(&self) -> bool {
        match &self.rows {
            Rows::Given(_) => false,
            Rows::Rendered(remote) => remote
                .source
                .try_borrow()
                .is_ok_and(|rows| rows.has_changes()),
        }
    }

    fn tree_at(&self, index: usize) -> Option<TreeRow<Self::Key>> {
        if let Some(node) = self.removed_at(index) {
            return Some(TreeRow {
                parent: node.parent.clone(),
                depth: node.depth,
                above: None,
            });
        }
        match &self.rows {
            Rows::Given(nodes) => Some(TreeRow {
                parent: nodes.nodes.get(index)?.parent.clone(),
                depth: nodes.nodes.get(index)?.depth,
                above: nodes.nodes.get(index)?.above.clone(),
            }),
            Rows::Rendered(remote) => {
                let key = remote.source.try_borrow().ok()?.key_at(index)?;
                let node = remote.node(&key)?;
                Some(TreeRow {
                    parent: node.parent.clone(),
                    depth: node.depth,
                    above: node.above.clone(),
                })
            }
        }
    }

    fn accepts_children(&self, index: usize) -> bool {
        self.has_subtasks(index)
    }

    fn has_subtasks(&self, index: usize) -> bool {
        if self.removed_at(index).is_some() {
            return false;
        }
        match &self.rows {
            Rows::Given(nodes) => nodes.has_subtasks(index),
            Rows::Rendered(remote) => {
                let Ok(rows) = remote.source.try_borrow() else {
                    return false;
                };
                let Some(key) = rows.key_at(index) else {
                    return false;
                };
                drop(rows);
                remote.node(&key).is_some_and(|node| node.branch)
            }
        }
    }

    fn discloses(&self) -> bool {
        true
    }

    fn label(&self, index: usize) -> Option<String> {
        if let Some(node) = self.removed_at(index) {
            return Some(node.label.clone());
        }
        match &self.rows {
            Rows::Given(nodes) => nodes.label(index),
            Rows::Rendered(remote) => {
                let key = remote.source.try_borrow().ok()?.key_at(index)?;
                Some(remote.node(&key)?.label.clone())
            }
        }
    }

    fn key_presenting(&self, id: &str) -> Option<Self::Key> {
        self.key_of_item(id)
    }
}

/// The row of the node at `key`: its list row, indented to where it stands, with what its row
/// part says of it.
fn row_of(
    rows: &TreeRows,
    key: &str,
    feel: crate::motion::Feel,
    marks: &TreeMarks<String>,
    matching: Option<&HashSet<String>>,
    chevron_column: f64,
) -> Element {
    let Some(node) = rows.node(key) else {
        return Box::new(ListRow::new(key.to_owned(), ""));
    };
    let context = matching.is_some_and(|found| !found.contains(node.item.0.as_str()));
    let key_owned = key.to_owned();
    let mut row = ListRow::new(key_owned.clone(), node.label.clone())
        .depth(marks.depth(&key_owned))
        .chevron_column(chevron_column)
        .row_height(TREE_ROW_HEIGHT)
        .with_feel(feel);
    if context {
        row = row.faint();
    }
    if let Some(props) = &node.row {
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
        if let Some(text) = &props.description {
            row = row.description(text.clone());
        }
    }
    if let Some(progress) = &node.progress {
        row = row.meta(progress.clone());
    }
    if node.removed {
        row = row.struck();
    }
    if let Some(mark) = node.change.as_ref().and_then(|mark| mark.again().row()) {
        row = row.change(mark.identity, mark.name, mark.spoken, mark.said, mark.show);
    }
    if let Some(person) = &node.presence {
        row = row.presence(person.identity.clone());
    }
    let row = Box::new(row) as Element;
    if node.toolbar.is_empty() {
        row
    } else {
        let toolbar: Vec<Element> = node
            .toolbar
            .iter()
            .enumerate()
            .map(|(at, props)| {
                let button = Button::from_props(&format!("{key}~action-{at}"), props.clone());
                Box::new(button) as Element
            })
            .collect();
        Box::new(HoverToolbar::new(row, toolbar))
    }
}

impl DescribedTreeView {
    /// The rows the tree was built from.
    #[cfg(test)]
    fn rows(&self) -> &TreeRows {
        &self.rows
    }

    /// The nodes the tree was given whole: what its rows are, read once.
    #[cfg(test)]
    fn nodes(&self) -> &Nodes {
        match &self.rows.rows {
            Rows::Given(nodes) => nodes,
            Rows::Rendered(_) => panic!("a tree whose host renders its rows holds none"),
        }
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
        self.rows.key_of_item(item)
    }

    /// The rows selected, by key.
    #[cfg(test)]
    pub(crate) fn selected(&self) -> Vec<String> {
        self.list.selection().stored_keys().cloned().collect()
    }

    /// The resource the node at `key` stands for.
    #[cfg(test)]
    pub(crate) fn item_of(&self, key: &str) -> Option<&ResourceId> {
        self.nodes().resource_of(key)
    }

    /// The tree of `props`: its rows, and what it reports of them. `navigated` is what it tells
    /// the window hosting it of the row the person opened, kept across the builds of a tree its
    /// view changed.
    fn build(key: &str, props: TreeViewProps, navigated: Rc<Cell<bool>>) -> Self {
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
            removed,
            expanded,
            matching,
            selection,
            motion,
            select,
            activate,
            drop,
            into,
            after,
        } = props;
        let feel = motion.unwrap_or(crate::motion::Feel::Standard);
        let marks = Rc::new(TreeMarks::<String>::default());
        let rows = TreeRows::of(
            nodes,
            removed,
            hide_root,
            name.clone(),
            expanded.as_deref(),
            Moves { drop, into, after },
            &marks,
        );
        let row_marks = Rc::clone(&marks);
        let found: Option<Rc<HashSet<String>>> =
            matching.map(|found| Rc::new(found.iter().map(|id| id.0.clone()).collect()));
        let mut list = VirtualList::with_selection_delegate(
            rows.clone(),
            TREE_ROW_HEIGHT,
            move |rows, _, key, build| {
                row_of(
                    rows,
                    key,
                    feel,
                    &row_marks,
                    found.as_deref(),
                    f64::from(build.chevron_column),
                )
            },
        )
        .with_tree(Rc::clone(&marks))
        .with_id(key)
        .with_semantic_key(|key: &String| format!("node:{key}"));
        if let Some(name) = &name {
            list = list.with_accessible_name(name.clone());
        }
        list = if fill {
            list.filling().in_column()
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
        let reporting = rows.clone();
        let picking = rows.clone();
        let select_single = single_click;
        let selected = select.clone();
        let opening = Rc::clone(&navigated);
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
                opening.set(true);
                let value = picking.value_of(&key);
                if select_single && by_hand() {
                    select.fire(value.clone());
                }
                activate.fire(value);
            });
        if rows.has_menu() {
            let menus = rows.clone();
            list = list.with_context_menu(
                move |_: &TreeRows, key: Option<&String>| menus.menu_of(key),
                |_, _| {},
            );
        }
        if reorder {
            let moves = rows.clone();
            list = list.with_reorder(
                |rows: &TreeRows, source: &String, target: &DropTarget<String>| {
                    rows.can_reorder(source, target)
                },
                move |_: &mut TreeRows, dropped: ReorderDrop<String>| {
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
                if let Some(key) = rows.key_of_item(&id.0) {
                    chosen.toggle(key);
                }
            }
            list = list.with_selection(chosen);
        }
        Self {
            key: key.to_owned(),
            list,
            rows,
            marks,
            own_folds: None,
            navigated,
        }
    }
}

impl Described for DescribedTreeView {
    type Props = TreeViewProps;

    fn from_props(key: &str, props: TreeViewProps) -> Self {
        Self::build(key, props, Rc::new(Cell::new(false)))
    }

    /// The rows are built again with what the person kept: a branch they folded stays folded,
    /// what they selected stays selected, the row they were on stays focused and where they
    /// were in it stays. A change of a node's label or place is a row the list draws, so
    /// nothing narrower saves the rebuild.
    ///
    /// A view that says where the tree is (`expanded`) shows the branches it says open while it
    /// says it: what the person had folded before is kept aside, and comes back when the view
    /// stops saying it, as a search that widens again shows the tree the person left.
    fn set_props(&mut self, props: TreeViewProps) -> bool {
        let motion = props.motion;
        let folded = self.marks.folded_keys();
        let selected: Vec<String> = self.list.selection().stored_keys().cloned().collect();
        let focused = self.list.focused_key().cloned();
        let scroll = self.list.scroll_offset();
        let keep_folded = props.expanded.is_none();
        let said = props.expanded.is_some();
        let kept = self.own_folds.take();
        let keep_selected = props.selection.is_none();
        let key = self.key.clone();
        let navigated = Rc::clone(&self.navigated);
        *self = Self::build(&key, props, navigated);
        match (said, kept) {
            (true, kept) => self.own_folds = Some(kept.unwrap_or(folded)),
            (false, Some(kept)) => self.marks.restore_folded(kept),
            (false, None) if keep_folded => self.marks.restore_folded(folded),
            (false, None) => {}
        }
        if keep_selected {
            let mut chosen = KeySelection::new();
            for key in selected {
                if self.rows.holds(&key) {
                    chosen.toggle(key);
                }
            }
            *self.list.selection_mut() = chosen;
        }
        if let Some(key) = focused {
            if self.rows.holds(&key) {
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

    /// The tree tells the window hosting it that the person opened a row of it, as a list of
    /// places does: what a narrow window's drawer closes on.
    fn provide<'a>(&'a self, request: &mut crate::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn NavigationSource>() {
            request.provide::<dyn NavigationSource>(self);
            return;
        }
        self.list.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut crate::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn NavigationSource>() {
            request.provide::<dyn NavigationSource>(self);
            return;
        }
        self.list.provide_mut(request);
    }
}

impl NavigationSource for DescribedTreeView {
    fn take_navigation(&mut self) -> bool {
        self.navigated.replace(false)
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
            ]
            .into(),
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
    use crate::Theme;
    use crate::component::{BoundValue, Fired, Intent};
    use crate::headless::{FrameHarness, InspectionNode};
    use crate::widget::TreeMove;

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
            check: Event::View(Intent::new(queue, key, "check")),
            ..TreeViewNodeProps::default()
        }
    }

    /// The three moves a tree reports, each on the tree itself.
    fn moves(queue: &crate::Intents<Fired>) -> Moves {
        Moves {
            drop: Event::View(Intent::new(queue, "tree", "drop")),
            into: Event::View(Intent::new(queue, "tree", "into")),
            after: Event::View(Intent::new(queue, "tree", "after")),
        }
    }

    fn tree_of(nodes: Vec<TreeViewNodeProps>, hide_root: bool, moves: Moves) -> DescribedTreeView {
        DescribedTreeView::from_props(
            "tree",
            TreeViewProps {
                hide_root,
                nodes: nodes.into(),
                drop: moves.drop,
                into: moves.into,
                after: moves.after,
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
            Moves::default(),
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
        let top: Vec<&str> = nodes
            .nodes
            .iter()
            .filter(|node| node.parent.is_none())
            .map(|node| node.key.as_str())
            .collect();
        assert_eq!(top, ["b", "itself"]);
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
                ]
                .into(),
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
                ]
                .into(),
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

    /// A view that says which branches stand open — a search opens the way to what it found —
    /// says nothing of the person's own folding: what they had opened is open again when the
    /// view stops saying it.
    #[test]
    fn a_tree_the_view_narrows_keeps_the_person_s_own_folding() {
        let queue = crate::Intents::default();
        let nodes = || {
            vec![
                node("plan", None, true, &queue),
                node("standups", Some("plan"), true, &queue),
                node("monday", Some("standups"), false, &queue),
                node("home", None, true, &queue),
                node("groceries", Some("home"), false, &queue),
            ]
        };
        let props = |expanded: Option<Vec<ResourceId>>| TreeViewProps {
            hide_root: true,
            nodes: nodes().into(),
            expanded,
            ..TreeViewProps::default()
        };
        let mut tree = DescribedTreeView::from_props("tree", props(None));
        tree.marks.fold(&"home".to_owned(), true);
        assert!(
            tree.is_open("plan"),
            "nothing is folded but what the person folded"
        );
        let found = vec![ResourceId("plan".into()), ResourceId("standups".into())];
        tree.set_props(props(Some(found)));
        assert!(tree.is_open("plan") && tree.is_open("standups"));
        assert!(
            !tree.is_open("home"),
            "a branch the search does not open stands folded"
        );
        tree.set_props(props(None));
        assert!(tree.is_open("plan"), "the branch they were reading is open");
        assert!(
            !tree.is_open("home"),
            "the branch they had folded is still folded, not the search's folding",
        );
    }

    /// A move is told to the tree, with the row that moved and the row it lands at: the row it
    /// goes before, the row it goes inside of, the row it goes after — with all of that row's
    /// own — or the end of the top level, which names no row to land at.
    #[test]
    fn a_move_names_the_row_that_moved_and_the_row_it_lands_at() {
        let queue = crate::Intents::default();
        let tree = tree_of(
            vec![
                node("a", None, true, &queue),
                node("b", Some("a"), false, &queue),
                node("c", None, false, &queue),
            ],
            true,
            moves(&queue),
        );
        let nodes = tree.rows();
        let landed = |moved: &str, at: &str, event: &str| Fired {
            key: "tree".into(),
            event: event.into(),
            value: Some(Value::List(vec![
                Value::Resource(moved.to_owned()),
                Value::Resource(at.to_owned()),
            ])),
        };

        assert!(nodes.report(&ReorderDrop {
            source: "c".to_owned(),
            target: DropTarget::new("b".to_owned(), DropPlacement::Before),
        }));
        assert_eq!(queue.take(), [landed("c", "b", "drop")]);

        assert!(nodes.report(&ReorderDrop {
            source: "c".to_owned(),
            target: DropTarget::new("a".to_owned(), DropPlacement::Inside),
        }));
        assert_eq!(queue.take(), [landed("c", "a", "into")]);

        // // After a row: the row itself is named, so the owner places it past that row's own // without
        // the tree having to find the row after them, which may be one it never built.
        assert!(nodes.report(&ReorderDrop {
            source: "c".to_owned(),
            target: DropTarget::new("a".to_owned(), DropPlacement::After),
        }));
        assert_eq!(queue.take(), [landed("c", "a", "after")]);

        assert!(nodes.report(&ReorderDrop {
            source: "b".to_owned(),
            target: DropTarget::new(String::new(), DropPlacement::After),
        }));
        assert_eq!(
            queue.take(),
            [Fired {
                key: "tree".into(),
                event: "drop".into(),
                value: Some(Value::List(vec![
                    Value::Resource("b".to_owned()),
                    Value::None,
                ])),
            }]
        );
    }

    /// A row's quick actions ride over its trailing corner while the row is hovered, as a list
    /// row's do: the toolkit's buttons, built again as the row shows.
    #[test]
    fn a_nodes_quick_actions_ride_over_its_row() {
        use crate::headless::FrameHarness;
        use crate::theme::Theme;

        fn names(node: &crate::headless::InspectionNode, out: &mut Vec<String>) {
            out.push(node.widget.clone());
            for child in &node.children {
                names(child, out);
            }
        }
        let queue = crate::Intents::default();
        let mut quick = node("a", None, true, &queue);
        quick.toolbar = vec![ButtonProps {
            label: "New sub-note".into(),
            icon: Some(crate::icons::Icon::Plus),
            icon_only: true,
            press: Event::View(Intent::new(&queue, "a", "quick")),
            ..ButtonProps::default()
        }];
        let tree = tree_of(vec![quick], true, Moves::default());
        let harness = FrameHarness::for_root(crate::State::new(), tree, (400, 240), Theme::dark());
        let mut laid = Vec::new();
        names(&harness.inspection().root, &mut laid);
        assert!(
            laid.iter().any(|widget| widget == "HoverToolbarFace"),
            "the row carries a hover toolbar: {laid:?}"
        );
        assert!(
            laid.iter().any(|widget| widget == "Button"),
            "with its quick action in it: {laid:?}"
        );
    }

    /// How far a branch got is the owner's count, said of the node: its row says it whether
    /// the branch is folded or open, as a list row of the same item does. A node with no count
    /// says none.
    #[test]
    fn a_node_says_how_far_its_branch_got() {
        let queue = crate::Intents::default();
        let mut said = node("a", None, true, &queue);
        said.progress = Some("1/3".into());
        said.row = Some(TreeViewRowProps::default());
        let mut child = node("b", Some("a"), false, &queue);
        child.row = Some(TreeViewRowProps {
            checkbox: Some(true),
            ..TreeViewRowProps::default()
        });
        let tree = tree_of(vec![said, child], true, Moves::default());
        let harness = FrameHarness::for_root(crate::State::new(), tree, (400, 240), Theme::dark());
        fn read(node: &InspectionNode, out: &mut Vec<(String, Option<String>)>) {
            if let Some(access) = &node.semantics
                && let Some(name) = &access.name
            {
                out.push((name.clone(), access.value.clone()));
            }
            for child in &node.children {
                read(child, out);
            }
        }
        let mut values = Vec::new();
        read(&harness.inspection().root, &mut values);
        let value = |words: &str| {
            values
                .iter()
                .find(|(name, _)| name == words)
                .and_then(|(_, value)| value.clone())
        };
        assert_eq!(value("a").as_deref(), Some("1/3"), "the row says the count");
        assert_eq!(value("b"), None, "a node with no count says none");
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
            Moves::default(),
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
                Moves {
                    drop: select.clone(),
                    ..Moves::default()
                },
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
            Moves::default(),
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

    /// A tree that fills its pane is the pane's list: its rows stand on the pane's content edge,
    /// their lift bleeding past it, as every list in a column does — so a tree standing as a
    /// phone window's screen keeps the page gutter at the glass rather than running to it. A tree
    /// whose first level folds keeps the chevron column on that edge, as the first thing standing
    /// there, and its rows' content one step past it.
    #[test]
    fn a_tree_that_fills_its_pane_stands_its_rows_on_the_panes_content_edge() {
        use crate::headless::{FrameHarness, InspectionNode};
        use crate::theme::Theme;
        use crate::widget::{LayoutInfo, RowLayout};

        // // Where the top-level row's box and its content start, and where its chevron column // stands,
        // for a tree of `nodes` in a 320 px window.
        fn column(fill: bool, nodes: Vec<TreeViewNodeProps>) -> (f64, f64, Option<f64>) {
            let folding = nodes.iter().any(|node| node.branch);
            let keys: Vec<String> = nodes.iter().map(|node| node.label.clone()).collect();
            let tree = DescribedTreeView::from_props(
                "tree",
                TreeViewProps {
                    hide_root: true,
                    fill,
                    nodes: nodes.into(),
                    ..TreeViewProps::default()
                },
            );
            let harness =
                FrameHarness::for_root(crate::State::new(), tree, (320, 568), Theme::dark());
            fn row(node: &InspectionNode) -> Option<(f64, RowLayout)> {
                match &node.layout {
                    Some(LayoutInfo::Row(grid)) => Some((node.rect[0], *grid)),
                    _ => node.children.iter().find_map(row),
                }
            }
            let (box_x0, grid) = row(&harness.inspection().root).expect("the row is laid out");
            let content = f64::from(grid.content_left);
            let chevron = folding.then(|| {
                harness
                    .root::<DescribedTreeView>()
                    .list
                    .disclosure_rect(&keys[0])
                    .expect("the branch shows its disclosure")
                    .x0
            });
            (box_x0, content, chevron)
        }
        let queue = crate::Intents::default();
        let theme = Theme::dark();
        let gutter = crate::chrome::column_gutter(&theme);
        let step = f64::from(crate::widget::TREE_INDENT);

        let (flat_box, flat, chevron) = column(true, vec![node("A note", None, false, &queue)]);
        assert!(
            (flat_box - gutter).abs() < 0.5 && (flat - gutter).abs() < 0.5,
            "a tree filling its pane starts its rows' boxes and their content on the pane's \
             content edge, {gutter} px in: {flat_box} and {flat}"
        );
        assert_eq!(
            chevron, None,
            "a tree whose first level folds nothing keeps no column"
        );

        let (folded_box, folded, chevron) = column(
            true,
            vec![
                node("A branch", None, true, &queue),
                node("A leaf", None, false, &queue),
                node("Under the branch", Some("A branch"), false, &queue),
            ],
        );
        assert!(
            (folded_box - gutter).abs() < 0.5,
            "its rows' boxes still stand on the {gutter} px edge, so their lift keeps the pane's \
             gutter: {folded_box}"
        );
        assert!(
            (folded - (gutter + step)).abs() < 0.5,
            "and their content keeps room for the chevron column: {folded} px in, one {step} px \
             step past the {gutter} px edge"
        );
        let chevron = chevron.expect("the branch shows its disclosure");
        assert!(
            (chevron - gutter).abs() < 0.5,
            "the chevron column is the first thing on the pane's content edge: {chevron} against \
             {gutter}"
        );

        let bleed = crate::chrome::row_bleed(&theme);
        let (_, inside, chevron) = column(false, vec![node("A branch", None, true, &queue)]);
        assert!(
            (inside - (bleed.max(step))).abs() < 0.5,
            "a tree inside another scroll keeps its own edge as the gutter, the page it stands in \
             holding the window's: {inside} against {bleed} and {step}"
        );
        assert_eq!(
            chevron,
            Some(0.0),
            "so its chevron column stands on that edge, inside the page"
        );
    }

    /// A row of a tree holds what it shows: the box that ticks it off and the quick action over
    /// its corner are controls of the row, reached and worked under the one node it is.
    #[test]
    fn a_trees_row_holds_the_controls_it_shows() {
        use crate::a11y::Action;
        use crate::headless::FrameHarness;
        use crate::theme::Theme;
        use accesskit::Role as AkRole;

        let queue = crate::Intents::default();
        let mut props = node("a", None, true, &queue);
        props.row = Some(TreeViewRowProps {
            checkbox: Some(false),
            ..TreeViewRowProps::default()
        });
        props.toolbar = vec![ButtonProps {
            label: "Add".to_owned(),
            press: Event::View(Intent::new(&queue, "a", "add")),
            ..ButtonProps::default()
        }];
        let mut harness = FrameHarness::for_root(
            crate::State::new(),
            tree_of(vec![props], true, Moves::default()),
            (400, 400),
            Theme::dark(),
        );

        let nodes = harness.a11y_nodes();
        let node_of = |role: AkRole, label: &str| {
            nodes
                .iter()
                .find(|(_, node)| node.role() == role && node.label() == Some(label))
                .cloned()
        };
        let (row, _) = node_of(AkRole::TreeItem, "a").expect("the tree hands over its node");
        let mut controls = Vec::new();
        let mut frontier = vec![row];
        while let Some(next) = frontier.pop() {
            let children = nodes
                .iter()
                .find(|(id, _)| *id == next)
                .map(|(_, node)| node.children().to_vec())
                .unwrap_or_default();
            for child in children {
                controls.push(child);
                frontier.push(child);
            }
        }
        let label_of = |id: accesskit::NodeId| {
            nodes
                .iter()
                .find(|(each, _)| *each == id)
                .map(|(_, node)| node.clone())
                .expect("an exported node")
        };
        let checkbox = controls
            .iter()
            .map(|id| (*id, label_of(*id)))
            .find(|(_, node)| node.role() == AkRole::CheckBox)
            .expect("the row hands over its box");
        assert_eq!(checkbox.1.toggled(), Some(accesskit::Toggled::False));
        controls
            .iter()
            .map(|id| label_of(*id))
            .find(|node| node.role() == AkRole::Button && node.label() == Some("Add"))
            .expect("the row hands over its quick action");

        harness
            .perform_a11y_action(checkbox.0, &Action::Activate)
            .expect("the box is operated");
        let heard: Vec<_> = queue
            .take()
            .into_iter()
            .map(|fired| (fired.key, fired.event))
            .collect();
        assert_eq!(
            heard,
            [("a".to_owned(), "check".to_owned())],
            "the tick reaches the row's box, and the owner hears it"
        );
    }

    /// The list row the tree builds for the node `key`, read as the list row it is.
    fn row_face<R>(tree: &DescribedTreeView, key: &str, read: impl FnOnce(&ListRow) -> R) -> R {
        let element = row_of(
            tree.rows(),
            key,
            crate::motion::Feel::Standard,
            &tree.marks,
            None,
            0.0,
        );
        let row = element
            .as_any()
            .and_then(|any| any.downcast_ref::<ListRow>())
            .unwrap_or_else(|| panic!("the row of {key} is a list row"));
        read(row)
    }

    /// A tree row shows who else is at the item it stands for, as a list row shows it.
    #[test]
    fn a_tree_row_says_who_else_is_at_the_item() {
        let queue = crate::Intents::default();
        let mut props = node("a", None, false, &queue);
        props.presence = Some(Person {
            identity: "bo".to_owned(),
            name: "Bo".to_owned(),
            hue: crate::tokens::PresenceHue::of_identity("bo"),
        });
        let tree = tree_of(vec![props], true, Moves::default());
        assert_eq!(
            row_face(&tree, "a", |row| row
                .with_face(|face| face.presence_identity().map(str::to_owned))),
            Some("bo".to_owned()),
            "the row says who else is at the item"
        );
        let plain = tree_of(vec![node("b", None, false, &queue)], true, Moves::default());
        assert_eq!(
            row_face(&plain, "b", |row| row
                .with_face(|face| face.presence_identity().map(str::to_owned))),
            None,
            "and says nothing while nobody else is"
        );
    }

    /// A node someone removed that the person has not seen go stands struck, after the nodes,
    /// at the top level: the row it stood under is one the tree may never have built.
    #[test]
    fn a_removed_node_stands_struck_after_the_nodes() {
        let queue = crate::Intents::default();
        let kept = node("a", None, false, &queue);
        let gone = TreeViewNodeProps {
            item: ResourceId("gone".to_owned()),
            label: "Gone".to_owned(),
            removed: true,
            ..TreeViewNodeProps::default()
        };
        let tree = DescribedTreeView::from_props(
            "tree",
            TreeViewProps {
                hide_root: true,
                nodes: vec![kept].into(),
                removed: vec![gone].into(),
                ..TreeViewProps::default()
            },
        );
        let rows = tree.rows();
        assert_eq!(rows.len(), 2, "the node and the row standing in for it");
        let struck = rows.key_at(1).expect("the removed row");
        assert_eq!(
            rows.label(1).as_deref(),
            Some("Gone"),
            "the row says what it was called"
        );
        assert_eq!(
            rows.tree_at(1).and_then(|row| row.parent),
            None,
            "it stands at the top level"
        );
        assert!(
            !rows.has_subtasks(1),
            "a row standing in for what left holds nothing"
        );
        assert!(
            row_face(&tree, &struck, |row| row
                .with_face(|face| face.title_struck())),
            "its title is struck through"
        );
        let kept = rows.key_at(0).expect("the node");
        assert!(
            !row_face(&tree, &kept, |row| row
                .with_face(|face| face.title_struck())),
            "a row the person still has is not struck"
        );
    }

    /// A row of a tree whose host renders its rows as the list shows them.
    #[derive(Clone)]
    struct ShownRow {
        key: String,
        parent: Option<String>,
        /// The row the view says stands over this one under the same parent.
        above: Option<String>,
        depth: i64,
        branch: bool,
    }

    /// The rows of a tree a host renders as the list shows them: every row at hand, and only the
    /// rows of the window asked for rendered — a notebook of ten thousand notes gives the rows
    /// on screen and forgets the rest.
    struct ShownRows {
        rows: Vec<ShownRow>,
        shown: std::ops::Range<usize>,
        queue: crate::Intents<Fired>,
        /// The rows the host rendered anew, reported at the list's next poll.
        changed: Vec<ShownRow>,
    }

    impl ShownRows {
        fn at(&self, key: &str) -> Option<usize> {
            self.rows.iter().position(|row| row.key == key)
        }
    }

    /// A row read as its node: the props the view would give it.
    struct RowSource {
        key: String,
        props: Vec<(&'static str, Value)>,
        queue: crate::Intents<Fired>,
    }

    impl crate::component::Source for RowSource {
        fn key(&self) -> &str {
            &self.key
        }

        fn prop(&mut self, name: &str) -> Option<Value> {
            self.props
                .iter()
                .find(|(prop, _)| *prop == name)
                .map(|(_, value)| value.clone())
        }

        fn event(&mut self, name: &str) -> Event {
            if name == "check" {
                Event::View(Intent::new(&self.queue, &self.key, name))
            } else {
                Event::None
            }
        }

        fn binding(&mut self, _name: &str) -> Option<BoundValue> {
            None
        }

        fn children(&mut self, _slot: &str) -> Vec<crate::Element> {
            Vec::new()
        }

        fn components(
            &mut self,
            _slot: &str,
            _read: &mut dyn FnMut(&mut dyn crate::component::Source) -> Result<(), String>,
        ) -> Result<usize, String> {
            Ok(0)
        }
    }

    impl crate::component::RowSource for ShownRows {
        fn len(&self) -> usize {
            self.rows.len()
        }

        fn key_at(&self, index: usize) -> Option<String> {
            self.rows.get(index).map(|row| row.key.clone())
        }

        fn index_of(&self, key: &str) -> Option<usize> {
            self.at(key)
        }

        fn request_range(&mut self, range: std::ops::Range<usize>, _open: Option<String>) {
            if self.shown == range {
                return;
            }
            self.shown = range;
            self.changed = self.rows.clone();
        }

        fn rendered(&self, key: &str) -> bool {
            self.at(key).is_some_and(|at| self.shown.contains(&at))
        }

        fn read(
            &mut self,
            key: &str,
            read: &mut dyn FnMut(&mut dyn crate::component::Source) -> Result<(), String>,
        ) -> Result<bool, String> {
            let Some(at) = self.at(key) else {
                return Ok(false);
            };
            if !self.shown.contains(&at) {
                return Ok(false);
            }
            let row = self.rows[at].clone();
            let mut props = vec![
                ("item", Value::Resource(row.key.clone())),
                ("label", Value::Text(row.key.clone())),
                ("depth", Value::Int(row.depth)),
                ("branch", Value::Bool(row.branch)),
            ];
            for (name, item) in [("parent", row.parent), ("above", row.above)] {
                if let Some(item) = item {
                    props.push((name, Value::Resource(item)));
                }
            }
            read(&mut RowSource {
                key: row.key,
                props,
                queue: self.queue.clone(),
            })?;
            Ok(true)
        }

        fn key_presenting(&self, id: &str) -> Option<String> {
            self.at(id).map(|_| id.to_owned())
        }

        fn take_changes(&mut self) -> Vec<crate::widget::virtual_list::ListModelChange<String>> {
            if self.changed.is_empty() {
                return Vec::new();
            }
            vec![crate::widget::virtual_list::ListModelChange::RowsChanged {
                keys: std::mem::take(&mut self.changed)
                    .into_iter()
                    .map(|row| row.key)
                    .collect(),
            }]
        }

        fn has_changes(&self) -> bool {
            !self.changed.is_empty()
        }
    }

    /// The tree of `rows` as a host renders them: only the rows the window shows are read, and
    /// the queue its moves land in.
    fn shown_tree(
        rows: Vec<ShownRow>,
    ) -> (
        FrameHarness,
        crate::Intents<Fired>,
        std::rc::Rc<std::cell::RefCell<ShownRows>>,
    ) {
        let queue = crate::Intents::default();
        struct WithRows {
            rows: crate::component::SharedRows,
            queue: crate::Intents<Fired>,
        }
        impl crate::component::Source for WithRows {
            fn key(&self) -> &str {
                "tree"
            }

            fn prop(&mut self, name: &str) -> Option<Value> {
                match name {
                    "reorder" | "tab_indents" | "hide_root" | "single_click" | "fill" => {
                        Some(Value::Bool(true))
                    }
                    _ => None,
                }
            }

            fn event(&mut self, name: &str) -> Event {
                matches!(name, "drop" | "into" | "after")
                    .then(|| Event::View(Intent::new(&self.queue, "tree", name)))
                    .unwrap_or(Event::None)
            }

            fn binding(&mut self, _name: &str) -> Option<BoundValue> {
                None
            }

            fn children(&mut self, _slot: &str) -> Vec<crate::Element> {
                Vec::new()
            }

            fn components(
                &mut self,
                _slot: &str,
                _read: &mut dyn FnMut(&mut dyn crate::component::Source) -> Result<(), String>,
            ) -> Result<usize, String> {
                Ok(0)
            }

            fn rows(&mut self, slot: &str) -> Option<crate::component::SharedRows> {
                (slot == "nodes").then(|| std::rc::Rc::clone(&self.rows))
            }
        }
        let shown = std::rc::Rc::new(std::cell::RefCell::new(ShownRows {
            rows,
            shown: 0..0,
            queue: queue.clone(),
            changed: Vec::new(),
        }));
        let rows: crate::component::SharedRows = shown.clone();
        let mut source = WithRows {
            rows,
            queue: queue.clone(),
        };
        let props = TreeViewProps::from_source(&mut source).expect("the tree reads");
        let tree = DescribedTreeView::from_props("tree", props);
        let harness = FrameHarness::for_root(
            crate::State::new(),
            tree,
            (600, 400),
            crate::theme::Theme::light(),
        );
        (harness, queue, shown)
    }

    /// The move `movement` reports of the focused row, as the queue heard it.
    fn moved(
        harness: &mut FrameHarness,
        queue: &crate::Intents<Fired>,
        movement: TreeMove,
    ) -> Vec<(String, String, Option<Value>)> {
        harness
            .root_mut::<DescribedTreeView>()
            .list
            .move_focused_step(movement);
        queue
            .take()
            .into_iter()
            .map(|fired| (fired.key, fired.event, fired.value))
            .collect()
    }

    fn landing(moved: &str, at: &str, event: &str) -> (String, String, Option<Value>) {
        (
            "tree".to_owned(),
            event.to_owned(),
            Some(Value::List(vec![
                Value::Resource(moved.to_owned()),
                Value::Resource(at.to_owned()),
            ])),
        )
    }

    /// `Tab` puts a row under the one over it and `Shift-Tab` takes it back out, with the rows
    /// between them never built: a branch of forty notes over the row leaves its own off the
    /// window, and the view still says which row stands over which. The move names the branch,
    /// which the tree has not built either, so the owner places the row all the same.
    #[test]
    fn tab_and_shift_tab_move_a_row_whose_neighbours_stand_off_the_window() {
        let mut rows = vec![ShownRow {
            key: "a".to_owned(),
            parent: None,
            above: None,
            depth: 0,
            branch: true,
        }];
        for at in 0..200 {
            rows.push(ShownRow {
                key: format!("a{at}"),
                parent: Some("a".to_owned()),
                above: Some(if at == 0 {
                    "a".to_owned()
                } else {
                    format!("a{}", at - 1)
                }),
                depth: 1,
                branch: false,
            });
        }
        rows.push(ShownRow {
            key: "b".to_owned(),
            parent: None,
            above: Some("a".to_owned()),
            depth: 0,
            branch: false,
        });
        let (mut harness, queue, shown) = shown_tree(rows);
        harness.relayout();
        {
            let mut root = harness.root_mut::<DescribedTreeView>();
            assert!(root.list.reveal(&"b".to_owned()), "the row comes into view");
            root.list.set_focus(true);
        }
        harness.relayout();
        harness.relayout();
        let at = shown.borrow().at("b").expect("the last row");
        assert!(
            !shown.borrow().shown.contains(&0),
            "the branch over the row stands off the window: {:?}",
            shown.borrow().shown
        );
        assert!(at > 0);

        // // Tab puts it under the branch the view says stands over it: the branch's own rows // between
        // them were never built, and the branch itself was not either.
        assert_eq!(
            moved(&mut harness, &queue, TreeMove::Indent),
            [landing("b", "a", "into")],
            "Tab indents the row into the branch over it"
        );
        assert_eq!(moved(&mut harness, &queue, TreeMove::Outdent), []);
    }

    /// A row's branch can stand off the window as its parent does: `Shift-Tab` names it still,
    /// from what the row itself says of it, and the row comes out beside it.
    #[test]
    fn shift_tab_lifts_a_row_out_from_a_parent_the_window_does_not_show() {
        let mut rows = vec![ShownRow {
            key: "a".to_owned(),
            parent: None,
            above: None,
            depth: 0,
            branch: true,
        }];
        for at in 0..200 {
            rows.push(ShownRow {
                key: format!("a{at}"),
                parent: Some("a".to_owned()),
                above: Some(if at == 0 {
                    "a".to_owned()
                } else {
                    format!("a{}", at - 1)
                }),
                depth: 1,
                branch: false,
            });
        }
        let (mut harness, queue, shown) = shown_tree(rows);
        harness.relayout();
        {
            let mut root = harness.root_mut::<DescribedTreeView>();
            assert!(
                root.list.reveal(&"a199".to_owned()),
                "the row comes into view"
            );
            root.list.set_focus(true);
        }
        harness.relayout();
        harness.relayout();
        assert!(
            !shown.borrow().shown.contains(&0),
            "the parent's own row stands off the window: {:?}",
            shown.borrow().shown
        );
        assert_eq!(
            moved(&mut harness, &queue, TreeMove::Outdent),
            [landing("a199", "a", "after")],
            "the row comes out beside the parent it stood under"
        );
    }
}
