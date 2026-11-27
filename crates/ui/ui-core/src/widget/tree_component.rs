//! `TreeView` as a described component: a tree of nodes, each a part in a part, with its
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
//! ([`tilo_ui_base::component::RowSource`]), so a notebook of ten thousand notes costs the rows on
//! screen. What the list needs of a row it does not hold — whether it opens, how deep it
//! stands — a row says itself ([`TreeViewNodeProps::branch`], [`TreeViewNodeProps::depth`]), so
//! the window it shows needs no row outside it.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Deref;
use std::rc::Rc;

use crate::widget::nav_list::NavigationSource;
use crate::{
    widget::Button, widget::ButtonProps, widget::ChangeMarkProps, widget::HoverToolbar,
    widget::ListModel, widget::ListRow, widget::MenuItem, widget::MenuItemProps, widget::TreeMarks,
    widget::TreeRow, widget::VirtualList,
};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::selection::KeySelection;
use tilo_ui_base::{
    component::Accepts, component::Component, component::Described, component::Event,
    component::PartType, component::Person, component::ResourceId, component::SharedRows,
    component::Source, component::Value,
};
use tilo_ui_base::{reorder::DropPlacement, reorder::DropTarget, reorder::ReorderDrop};
use tilo_ui_base::{widget::Decorator, widget::Element, widget::Press, widget::Widget};

use crate::widget::virtual_list::by_hand;

/// The height of one row of a tree: an outline's rows are its own token, the compact row's
/// height, shorter than a list's row of the same theme.
pub const TREE_ROW_HEIGHT: f32 = tilo_ui_base::tokens::Density::Compact.rows().single;

/// The height a tree's row stands at when it carries a line beside its name: the compact row's
/// two-line height, so the line under the name is as tall as a list row's line.
pub const TREE_ROW_DOUBLE_HEIGHT: f32 = tilo_ui_base::tokens::Density::Compact.rows().double;

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

tilo_ui_base::choice!(TreeTone, "TreeTone", {
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
    /// Who else is at the item now (a member's or an agent's lens on it), shown on the
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
    #[component(part)]
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
    #[component(part)]
    pub menu: Vec<MenuItemProps>,
    /// The controls the row shows over its trailing corner while it is hovered or holds focus:
    /// the quick action of the thing it stands for (a note's "+", a folder's), without opening a
    /// menu. A node that gives none shows no hover toolbar.
    #[component(part)]
    pub toolbar: Vec<ButtonProps>,
    /// What someone else changed of the row's resource that the person has not seen yet: the
    /// row shows it, in their colour, on its trailing edge. The UI host places it
    /// (task-legibility §3).
    #[component(part)]
    pub change: Option<ChangeMarkProps>,
}

tilo_ui_base::part_type!(TreeViewNodeProps);
tilo_ui_base::part_type!(TreeViewRowProps);
tilo_ui_base::part_type!(ButtonProps);

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

impl PartType for TreeViewRows {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(TreeViewNodeProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        if let Some(rows) = source.rows(part) {
            self.nodes = Held::Rendered(rows);
            return Ok(());
        }
        let mut given = Vec::new();
        source.components(part, &mut |child| {
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

impl PartType for RemovedNodes {
    const MANY: bool = true;

    fn accepts() -> Accepts {
        Accepts::Component(TreeViewNodeProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        let mut nodes: Vec<TreeViewNodeProps> = Vec::new();
        source.components(part, &mut |child| {
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
    #[component(part(default))]
    pub nodes: TreeViewRows,
    /// The rows in place of nodes someone else removed that the person has not seen go:
    /// they stand after the nodes, struck, each with its mark. The UI host places them.
    #[component(part)]
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
    pub motion: Option<tilo_ui_base::motion::Feel>,
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
pub const TOP_LEVEL: &str = "";

/// What a tree reports of a move: the row that moved and the row it lands at, by the resource
/// each shows. The tree's own, so a move lands whether or not the row it lands at was built.
#[derive(Clone, Default)]
pub struct Moves {
    /// A row lands just before another row, or at the end of the top level (none).
    pub drop: Event,
    /// A row lands inside another, at the end of its own.
    pub into: Event,
    /// A row lands just after another and all that stands under it.
    pub after: Event,
}

/// One row of a tree, its parts read once.
#[derive(Clone)]
pub struct Node {
    pub key: String,
    /// The resource it stands for, which its events carry.
    pub item: ResourceId,
    pub label: String,
    /// The row it stands under, as the list sees it: the tree's own row over a top-level node,
    /// or none where the tree hides that row.
    pub parent: Option<String>,
    /// How deep it stands, when the view says it: the list indents by it rather than by the
    /// rows over it.
    pub depth: Option<u32>,
    /// Whether it opens to show nodes under it, whether or not any of them stands in the list.
    pub branch: bool,
    /// Who else is at the item now, shown on the row.
    pub presence: Option<Person>,
    /// It stands in for an item someone removed, its title struck through.
    pub removed: bool,
    pub row: Option<TreeViewRowProps>,
    pub progress: Option<String>,
    pub menu: Vec<Command>,
    pub toolbar: Vec<ButtonProps>,
    pub change: Option<Rc<ChangeMarkProps>>,
    /// The row the view says stands just over this one under the same parent: the row `Tab`
    /// indents it under and `Alt-Up` goes before, held or not.
    pub above: Option<String>,
    pub check: Event,
}

/// Every node of a tree, and what a list of them answers.
///
/// Cheap to clone: a row builder, a menu and a move all read the same rows.
#[derive(Clone)]
pub struct Nodes {
    pub inner: Rc<Inner>,
}

/// A tree's rows are read through the one copy of them a list and its row builder share.
impl std::ops::Deref for Nodes {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.inner
    }
}

pub struct Inner {
    pub nodes: Vec<Node>,
    pub index: HashMap<String, usize>,
    /// The rows that open to show subtasks.
    pub branches: HashSet<String>,
    /// The key each resource stands at.
    pub keys: HashMap<String, String>,
    /// What the tree reports of a move: the row that moved, and the row it lands at.
    pub moves: Moves,
    /// Whether any node offers a menu, which the list offers of its rows only then.
    pub has_menu: bool,
    /// Whether any row shows a leading glyph, so every row keeps that column and the titles
    /// stand in one column.
    pub keeps_glyph: bool,
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
pub struct Command {
    label: String,
    icon: Option<Icon>,
    destructive: bool,
    checked: Option<bool>,
    key: Option<String>,
    group: Option<String>,
    enabled: bool,
    description: Option<String>,
    /// What choosing the row presses: the action the view bound to it, or the view's own event.
    press: Press,
}

impl Command {
    fn of(mut props: MenuItemProps) -> Self {
        let press = props.press();
        let MenuItemProps {
            label,
            icon,
            destructive,
            checked,
            disabled,
            description,
            group,
            key,
            // What choosing it does is the press, taken above.
            link: _,
            choose: _,
            value: _,
            separator: _,
            // A command row here is not a font picker's: no row previews a face.
            sample: _,
        } = props;
        Self {
            label,
            icon,
            destructive,
            checked,
            key,
            group,
            enabled: !disabled,
            description,
            press,
        }
    }

    fn entry(&self) -> MenuItem {
        let item = MenuItem::bound(self.label.clone(), self.press.clone())
            .with_destructive(self.destructive);
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
        let item = match &self.description {
            Some(description) => item.with_description(description.clone()),
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
    pub fn of(
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
        let keeps_glyph = nodes
            .iter()
            .any(|node| node.row.as_ref().is_some_and(|row| row.icon.is_some()));
        Self {
            inner: Rc::new(Inner {
                nodes,
                index,
                branches,
                keys: item_keys,
                moves,
                has_menu,
                keeps_glyph,
            }),
        }
    }

    fn node(&self, key: &str) -> Option<&Node> {
        self.index.get(key).and_then(|at| self.nodes.get(*at))
    }

    /// The key of the node for the resource `id`.
    fn key_of_item(&self, id: &str) -> Option<String> {
        self.keys.get(id).cloned()
    }

    /// The resource the node at `key` stands for.
    #[cfg(any(test, feature = "test-support"))]
    fn resource_of(&self, key: &str) -> Option<&ResourceId> {
        self.node(key).map(|node| &node.item)
    }

    /// Which rows start folded, given the rows the view says it holds open: every branch but
    /// those, and never the tree's own row, which holds the whole tree.
    pub fn folded_at_first(&self, expanded: &[ResourceId]) -> Vec<String> {
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
/// host renders as the list shows them, and after them the rows in place of nodes someone
/// else removed that the person has not seen go.
#[derive(Clone)]
pub struct TreeRows {
    pub rows: Rows,
    /// The struck rows of removed nodes, which the host gives whole: they stand after the rows,
    /// at the top level.
    pub removed: Rc<Vec<Node>>,
}

/// Where a tree's rows come from.
#[derive(Clone)]
pub enum Rows {
    /// The nodes the view gave whole, read once with what the list asks of them.
    Given(Nodes),
    /// The nodes a host renders as the list shows them, read as the list mounts each row.
    Rendered(Rc<Remote>),
}

/// The rows of a tree a host renders as the list shows them: a notebook of ten thousand notes
/// costs the rows on screen, and none stands in the list until it is shown.
pub struct Remote {
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
    /// Whether any row read so far shows a leading glyph, so every row keeps that column and the
    /// titles stand in one column. A row read before the first glyph keeps as it was until the
    /// list builds its rows again.
    glyph: std::cell::Cell<bool>,
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
            if node.row.as_ref().is_some_and(|row| row.icon.is_some()) {
                self.glyph.set(true);
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
    pub fn of(
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
                glyph: std::cell::Cell::new(false),
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

    /// The struck row at `index`, when it is one of the rows after the nodes.
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

    /// The key of the row for the resource `id`, when the tree holds one.
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

    /// Whether any row shows a leading glyph, so every row keeps its column.
    fn keeps_glyph(&self) -> bool {
        match &self.rows {
            Rows::Given(nodes) => nodes.keeps_glyph,
            Rows::Rendered(remote) => remote.glyph.get(),
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
    pub fn report(&self, dropped: &ReorderDrop<String>) -> bool {
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

    fn row_state(&self, index: usize) -> tilo_ui_base::arriving::Arrival<(), &'static str> {
        if let Rows::Rendered(remote) = &self.rows
            && let Ok(rows) = remote.source.try_borrow()
            && let Some(key) = rows.key_at(index)
            && !rows.rendered(&key)
        {
            return tilo_ui_base::arriving::Arrival::local();
        }
        tilo_ui_base::arriving::Arrival::Ready(())
    }

    fn poll_changes(
        &mut self,
    ) -> Vec<tilo_ui_base::collection_transition::ListModelChange<String>> {
        let Rows::Rendered(remote) = &self.rows else {
            return Vec::new();
        };
        let Ok(mut rows) = remote.source.try_borrow_mut() else {
            return Vec::new();
        };
        let changes = rows.take_changes();
        for change in &changes {
            match change {
                tilo_ui_base::collection_transition::ListModelChange::RowsChanged { keys } => {
                    remote.forget(keys);
                }
                tilo_ui_base::collection_transition::ListModelChange::Reset => remote.clear(),
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
#[allow(
    clippy::implicit_hasher,
    reason = "the tree's own rows and their folds are the one set a caller holds"
)]
pub fn row_of(
    rows: &TreeRows,
    key: &str,
    feel: tilo_ui_base::motion::Feel,
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
        .glyph_column(rows.keeps_glyph())
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
        row = row.change(mark.by, mark.spoken, mark.said, mark.show);
    }
    if let Some(person) = &node.presence {
        row = row.presence(person);
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
    #[cfg(any(test, feature = "test-support"))]
    pub fn rows(&self) -> &TreeRows {
        &self.rows
    }

    /// The nodes the tree was given whole: what its rows are, read once.
    /// The list the tree's rows stand in.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn test_list(&self) -> &VirtualList<TreeRows> {
        &self.list
    }

    /// The list the tree's rows stand in, to move the focus as a person would.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn test_list_mut(&mut self) -> &mut VirtualList<TreeRows> {
        &mut self.list
    }

    /// What the list works out of the rows as it mounts them.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn test_marks(&self) -> &Rc<TreeMarks<String>> {
        &self.marks
    }

    #[cfg(any(test, feature = "test-support"))]
    #[allow(
        clippy::panic,
        reason = "a tree whose host renders its rows holds none, which no test asks it for"
    )]
    pub fn nodes(&self) -> &Nodes {
        match &self.rows.rows {
            Rows::Given(nodes) => nodes,
            Rows::Rendered(_) => panic!("a tree whose host renders its rows holds none"),
        }
    }

    /// Whether the node at `key` is open.
    #[cfg(any(test, feature = "test-support"))]
    pub fn is_open(&self, key: &str) -> bool {
        !self.marks.is_folded(&key.to_owned())
    }

    /// The key of the row for the resource `item`, which is also the node's stable
    /// name on screen.
    #[cfg(any(test, feature = "test-support"))]
    pub fn row_key(&self, item: &str) -> Option<String> {
        self.rows.key_of_item(item)
    }

    /// The rows selected, by key.
    #[cfg(any(test, feature = "test-support"))]
    pub fn selected(&self) -> Vec<String> {
        self.list.selection().stored_keys().cloned().collect()
    }

    /// The resource the node at `key` stands for.
    #[cfg(any(test, feature = "test-support"))]
    pub fn item_of(&self, key: &str) -> Option<&ResourceId> {
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
        let feel = motion.unwrap_or(tilo_ui_base::motion::Feel::Standard);
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
            list.filling().in_column().overlay_scrollbar()
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
                    chosen.toggle(crate::widget::ListKey::new(key));
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
            .set_feel(motion.unwrap_or(tilo_ui_base::motion::Feel::Standard));
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
    fn take_structure_change(&mut self) -> tilo_ui_base::widget::StructureChange {
        self.list.take_structure_change()
    }

    // The tree tells the window hosting it that the person opened a row of it, as a list of
    // places does: what a narrow window's drawer closes on.
    tilo_ui_base::provides!(dyn NavigationSource; inner = list);
}

impl NavigationSource for DescribedTreeView {
    fn take_navigation(&mut self) -> bool {
        self.navigated.replace(false)
    }
}
