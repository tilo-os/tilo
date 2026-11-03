//! [`TreeView`] as a described component: a tree of nodes, each a part in a slot, with its
//! children named under it by key, and selecting or activating a node reported with the
//! resource it stands for.
//!
//! The nodes are [`TreeViewNodeProps`], one per row, each naming its parent by the resource it
//! stands for, and the
//! widget builds the hierarchy from the flat list: a node that changed is one part beside the
//! others, and the tree changes in place — an expanded branch folds where it stands, and the
//! selection follows what the view says. The widget's own disclosure, keyboard navigation and
//! focus stay its own.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::component::{Component, Described, Event, ResourceId, Value};
use crate::icons::Icon;
use crate::reorder::{DropPlacement, DropTarget, ReorderDrop};
use crate::widget::tree_view::{
    TreeClickActivation, TreeNode, TreeRowDecoration, TreeRowInfo, TreeRowTone, TreeView,
};
use crate::widget::{MenuItem, MenuItemProps, Widget};

/// What one row of a tree shows besides its label: a leading glyph, a tone, trailing text and
/// what assistive technology says of it.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "TreeViewRow", role = "Generic")]
pub struct TreeViewRowProps {
    /// A leading glyph: the kind of thing it is.
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

/// A described tree: its tree, where every node of it stands, and the widget id each node has.
pub struct DescribedTreeView {
    /// The widget id the tree was made with, kept so a change builds it again under the same
    /// id.
    key: String,
    tree: TreeView<()>,
    layout: Rc<Layout>,
}

/// The key of the top level in a layout's order: the parent of every node without one.
const TOP_LEVEL: &str = "";

/// One node of a layout, its parts read once.
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
    run: Rc<dyn Fn()>,
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
            run: choose.command().unwrap_or_else(|| Rc::new(|| {})),
        }
    }

    fn entry(&self) -> MenuItem {
        let run = Rc::clone(&self.run);
        let item = if self.destructive {
            MenuItem::destructive_action(self.label.clone(), move || run())
        } else {
            MenuItem::action(self.label.clone(), move || run())
        };
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

/// Where every node of a tree stands: the resource it stands for, its parent and the keys of
/// its children in order, and what it reports when a node lands on it. A move is reported by
/// the node the moved one lands at, so the owner names the place by its neighbour: the next
/// sibling it goes before, or the node it goes inside of.
pub struct Layout {
    ids: HashMap<String, u64>,
    items: HashMap<String, ResourceId>,
    labels: HashMap<String, String>,
    branch: HashSet<String>,
    parents: HashMap<String, Option<String>>,
    order: HashMap<String, Vec<String>>,
    rows: HashMap<String, TreeViewRowProps>,
    /// How far each branch got, as its node says of it: "1/3".
    progress: HashMap<String, String>,
    menus: HashMap<String, Vec<Command>>,
    drops: HashMap<String, Event>,
    intos: HashMap<String, Event>,
    checks: HashMap<String, Event>,
    root_drop: Event,
}

/// A node's widget id, from the resource it stands for: the same resource keeps its id when
/// the rows move or a label changes, so a branch a person folded stays folded and the row they
/// were on stays theirs.
fn node_id(key: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in key.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // Below the ids the tree reserves for its own rows, and exact in a layout coordinate.
    (hash >> 12) & 0x0fff_ffff
}

impl Layout {
    fn of(nodes: Vec<TreeViewNodeProps>, root_drop: Event) -> Self {
        let mut parts = Vec::with_capacity(nodes.len());
        for node in nodes {
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
        let mut ids: HashMap<String, u64> = HashMap::new();
        let mut taken: HashSet<u64> = HashSet::new();
        let mut items = HashMap::new();
        let mut labels = HashMap::new();
        let mut branch = HashSet::new();
        let mut parents = HashMap::new();
        let mut order: HashMap<String, Vec<String>> = HashMap::new();
        let mut rows = HashMap::new();
        let mut progress = HashMap::new();
        let mut menus = HashMap::new();
        let mut drops = HashMap::new();
        let mut intos = HashMap::new();
        let mut checks = HashMap::new();
        for part in parts {
            if items.contains_key(&part.key) {
                continue;
            }
            let mut id = node_id(&part.key);
            while id == 0 || taken.contains(&id) {
                id = id.wrapping_add(1) & 0x0fff_ffff;
            }
            taken.insert(id);
            ids.insert(part.key.clone(), id);
            // A parent the tree does not hold, or the node itself, leaves it at the top level.
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
            let under = parent.clone().unwrap_or_else(|| TOP_LEVEL.to_owned());
            order.entry(under).or_default().push(part.key.clone());
            parents.insert(part.key.clone(), parent);
            if part.branch {
                branch.insert(part.key.clone());
            }
            if let Some(row) = part.row {
                rows.insert(part.key.clone(), row);
            }
            if let Some(text) = part.progress {
                progress.insert(part.key.clone(), text);
            }
            if !part.menu.is_empty() {
                menus.insert(
                    part.key.clone(),
                    part.menu.into_iter().map(Command::of).collect(),
                );
            }
            if !part.drop.is_none() {
                drops.insert(part.key.clone(), part.drop);
            }
            if !part.into.is_none() {
                intos.insert(part.key.clone(), part.into);
            }
            if !part.check.is_none() {
                checks.insert(part.key.clone(), part.check);
            }
            labels.insert(part.key.clone(), part.label);
            items.insert(part.key, part.item);
        }
        // How far a branch got is the owner's to count, of the items it holds: it says it of
        // the node, and the tree shows it in the rows the app gave no wording of its own.
        Self {
            ids,
            items,
            labels,
            branch,
            parents,
            order,
            rows,
            progress,
            menus,
            drops,
            intos,
            checks,
            root_drop,
        }
    }

    /// The key of the node with the widget id `id`.
    fn key_of(&self, id: crate::widget::tree_view::NodeId) -> Option<&str> {
        self.ids
            .iter()
            .find(|(_, held)| **held == id)
            .map(|(key, _)| key.as_str())
    }

    /// The rows' props, by widget id: a branch says how far it got where its row does not say
    /// it itself.
    fn rows(&self) -> HashMap<crate::widget::tree_view::NodeId, TreeViewRowProps> {
        self.rows
            .iter()
            .filter_map(|(key, row)| {
                let mut row = row.clone();
                if row.progress.is_none() {
                    row.progress = self.progress.get(key).cloned();
                }
                Some((*self.ids.get(key)?, row))
            })
            .collect()
    }

    /// Every branch of the tree: what a tree opens to show when nothing says otherwise.
    fn branches(&self) -> impl Iterator<Item = crate::widget::tree_view::NodeId> + '_ {
        self.branch
            .iter()
            .filter_map(|key| self.ids.get(key).copied())
    }

    fn siblings_of(&self, parent: Option<&str>) -> &[String] {
        self.order
            .get(parent.unwrap_or(TOP_LEVEL))
            .map_or(&[], Vec::as_slice)
    }

    /// The node part as one of the widget's own nodes, its children under it.
    fn tree_node(&self, key: &str) -> TreeNode<()> {
        let id = self.ids.get(key).copied().unwrap_or(0);
        let children: Vec<TreeNode<()>> = self
            .siblings_of(Some(key))
            .iter()
            .map(|child| self.tree_node(child))
            .collect();
        let label = self.labels.get(key).cloned().unwrap_or_default();
        if self.branch.contains(key) || !children.is_empty() {
            TreeNode::branch(id, label, (), children)
        } else {
            TreeNode::leaf(id, label, ())
        }
    }

    /// The tree's root: a node of the tree's own name over the nodes at the top level. The
    /// view hides it ([`TreeView::hide_root`]) when the heading above the tree already says
    /// what the tree is, so the name is stated once.
    fn root(&self, name: Option<&String>) -> TreeNode<()> {
        let roots = self.siblings_of(None);
        TreeNode::branch(
            0,
            name.cloned().unwrap_or_default(),
            (),
            roots.iter().map(|key| self.tree_node(key)).collect(),
        )
    }

    /// Where a dragged or keyed move lands, reported by the node it lands at: the node it goes
    /// before, the node it goes inside of, or the end of the top level. Returns whether the
    /// owner was told.
    fn report(&self, dropped: &ReorderDrop<crate::widget::tree_view::NodeId>) -> bool {
        let Some(moved) = self
            .key_of(dropped.source)
            .and_then(|key| self.items.get(key))
        else {
            return false;
        };
        let value = Value::Resource(moved.0.clone());
        let (parent, next) = match self.key_of(dropped.target.key) {
            Some(key) => match dropped.target.placement {
                DropPlacement::Inside => (
                    Some(key.to_owned()),
                    self.siblings_of(Some(key)).first().cloned(),
                ),
                DropPlacement::Before => (
                    self.parents.get(key).cloned().flatten(),
                    Some(key.to_owned()),
                ),
                DropPlacement::After => {
                    let parent = self.parents.get(key).cloned().flatten();
                    let siblings = self.siblings_of(parent.as_deref());
                    let next = siblings
                        .iter()
                        .position(|sibling| sibling == key)
                        .and_then(|at| at.checked_add(1))
                        .and_then(|at| siblings.get(at))
                        .cloned();
                    (parent, next)
                }
            },
            // The tree's own root row: the top level's end.
            None => (None, None),
        };
        if let Some(next) = next {
            let Some(event) = self.drops.get(&next) else {
                return false;
            };
            event.fire(Some(value));
            return true;
        }
        if let Some(parent) = parent {
            let Some(event) = self.intos.get(&parent) else {
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
}

fn decoration(
    row: &TreeViewRowProps,
    info: &TreeRowInfo,
    theme: &crate::theme::Theme,
) -> TreeRowDecoration {
    let mut decoration = TreeRowDecoration::new();
    if let Some(icon) = row.icon {
        decoration = decoration.icon(icon, theme.text_primary);
    }
    decoration = decoration.tone(match row.tone.unwrap_or_default() {
        TreeTone::Primary => TreeRowTone::Primary,
        TreeTone::Secondary => TreeRowTone::Secondary,
    });
    if let Some(text) = &row.secondary {
        decoration = decoration.secondary(text.clone());
    }
    if let Some(text) = &row.trailing {
        decoration = decoration.trailing(text.clone());
    }
    // How far a branch got is said while it is closed; open, its rows say it themselves.
    if let Some(text) = &row.progress
        && info.is_branch
        && !info.expanded
    {
        decoration = decoration.progress(text.clone());
    }
    if let Some(checked) = row.checkbox {
        decoration = decoration.checkbox(checked);
    }
    if let Some(text) = &row.description {
        decoration = decoration.description(text.clone());
    }
    decoration
}

impl DescribedTreeView {
    /// The tree, for a test of what it shows and keeps.
    #[must_use]
    pub fn tree(&self) -> &TreeView<()> {
        &self.tree
    }

    /// The tree, to change.
    pub fn tree_mut(&mut self) -> &mut TreeView<()> {
        &mut self.tree
    }

    /// The widget id of the node with `key`.
    #[must_use]
    pub fn id_of(&self, key: &str) -> Option<u64> {
        self.layout.ids.get(key).copied()
    }

    /// The resource the node with `key` stands for.
    #[must_use]
    pub fn item_of(&self, key: &str) -> Option<&ResourceId> {
        self.layout.items.get(key)
    }

    /// The tree of `props`: its nodes, and what it reports of them.
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
        let layout = Rc::new(Layout::of(nodes, drop));
        let root = layout.root(name.as_ref());
        let mut tree = TreeView::new(root)
            .with_id(key)
            .with_feel(motion.unwrap_or(crate::motion::Feel::Standard));
        if let Some(name) = &name {
            tree = tree.with_accessible_name(name.clone());
        }
        if fill {
            tree = tree.fill();
        }
        if hide_root {
            tree = tree.hide_root();
        }
        if single_click {
            tree = tree.click_activation(TreeClickActivation::SingleClick);
        }
        if selection_follows_focus {
            tree = tree.selection_follows_focus(true);
        }
        if multi_select {
            tree = tree.multi_select(true);
        }
        let rows = Rc::new(layout.rows());
        let row_map = Rc::clone(&rows);
        tree = tree.with_row_decoration(
            move |_: &TreeNode<()>,
                  info: crate::widget::TreeRowInfo,
                  theme: &crate::theme::Theme| {
                row_map
                    .get(&info.id)
                    .map_or_else(TreeRowDecoration::new, |row| decoration(row, &info, theme))
            },
        );
        if !select.is_none() {
            let layout = Rc::clone(&layout);
            tree = tree.on_select(move |id: crate::widget::tree_view::NodeId| {
                if let Some(item) = layout.key_of(id).and_then(|key| layout.items.get(key)) {
                    select.fire(Some(Value::Resource(item.0.clone())));
                }
            });
        }
        if !activate.is_none() {
            let layout = Rc::clone(&layout);
            tree = tree.on_activate(move |id: crate::widget::tree_view::NodeId| {
                if let Some(item) = layout.key_of(id).and_then(|key| layout.items.get(key)) {
                    activate.fire(Some(Value::Resource(item.0.clone())));
                }
            });
        }
        if !layout.checks.is_empty() {
            let checkers = Rc::clone(&layout);
            tree = tree.on_check(move |id: crate::widget::tree_view::NodeId| {
                if let Some(event) = checkers.key_of(id).and_then(|key| checkers.checks.get(key)) {
                    event.fire(None);
                }
            });
        }
        if !layout.menus.is_empty() {
            let menus = Rc::clone(&layout);
            tree = tree.with_context_menu(
                move |_: &TreeNode<()>, target: Option<crate::widget::tree_view::NodeId>| {
                    target
                        .and_then(|id| menus.key_of(id))
                        .and_then(|key| menus.menus.get(key))
                        .map(|commands| commands.iter().map(Command::entry).collect())
                        .unwrap_or_default()
                },
            );
        }
        if reorder {
            let moves = Rc::clone(&layout);
            tree = tree.with_reorder(
                |model: &TreeNode<()>,
                 source: crate::widget::tree_view::NodeId,
                 target: &DropTarget<crate::widget::tree_view::NodeId>| {
                    model.can_reorder(source, target)
                },
                move |_: &mut TreeNode<()>,
                      dropped: ReorderDrop<crate::widget::tree_view::NodeId>| {
                    // The owner moves it; the tree shows what the model then says.
                    moves.report(&dropped);
                    false
                },
            );
            if tab_indents {
                tree = tree.tab_indents(true);
            }
        }
        if let Some(open) = &expanded {
            let wanted: Vec<u64> = open
                .iter()
                .filter_map(|key| layout.ids.get(&key.0).copied())
                .collect();
            tree = tree.with_expanded(wanted);
        } else {
            tree = tree.with_expanded(std::iter::once(0).chain(layout.branches()));
        }
        if let Some(selected) = &selection {
            let mut chosen = crate::selection::KeySelection::<u64>::new();
            for key in selected {
                if let Some(id) = layout.ids.get(&key.0).copied() {
                    chosen.toggle(id);
                }
            }
            tree = tree.with_selection(chosen);
        }
        Self {
            key: key.to_owned(),
            tree,
            layout,
        }
    }

    /// What the tree had open, by the resource of each node.
    fn opened(&self) -> Vec<String> {
        self.tree
            .expanded_ids()
            .filter_map(|id| self.layout.key_of(id).map(str::to_owned))
            .collect()
    }

    /// What the tree had selected, by the resource of each node.
    fn selected(&self) -> Vec<String> {
        let selection = self.tree.selection();
        if selection.is_select_all() {
            return Vec::new();
        }
        selection
            .stored_keys()
            .filter_map(|id| self.layout.key_of(*id).map(str::to_owned))
            .collect()
    }
}

impl Described for DescribedTreeView {
    type Props = TreeViewProps;

    fn from_props(key: &str, props: TreeViewProps) -> Self {
        Self::build(key, props)
    }

    /// The tree is built again with what it held: a branch a person folded stays folded, what
    /// they selected stays selected, the row they were on stays focused and where they were in
    /// it stays. A change of a node's label or place, or of what its row shows, is a row the
    /// tree draws, so nothing narrower saves the rebuild.
    fn set_props(&mut self, props: TreeViewProps) -> bool {
        let motion = props.motion;
        let opened = self.opened();
        let selected = self.selected();
        let focused = self.tree.focused();
        let scroll = self.tree.scroll_offset();
        let keep_opened = props.expanded.is_none();
        let keep_selected = props.selection.is_none();
        let key = self.key.clone();
        *self = Self::build(&key, props);
        if keep_opened {
            let ids: Vec<u64> = opened
                .iter()
                .filter_map(|key| self.layout.ids.get(key).copied())
                .collect();
            self.tree.restore_expanded(ids);
        }
        if keep_selected {
            let mut chosen = crate::selection::KeySelection::<u64>::new();
            for key in &selected {
                if let Some(id) = self.layout.ids.get(key).copied() {
                    chosen.toggle(id);
                }
            }
            *self.tree.selection_mut() = chosen;
        }
        if let Some(id) = focused
            .and_then(|id| self.layout.key_of(id).map(str::to_owned))
            .and_then(|key| self.layout.ids.get(&key).copied())
        {
            self.tree.set_focused(Some(id));
        }
        if scroll > 0.0 {
            self.tree.set_scroll_offset(scroll);
        }
        self.tree
            .set_feel(motion.unwrap_or(crate::motion::Feel::Standard));
        true
    }
}

/// A tree view is its tree.
impl crate::widget::Decorator for DescribedTreeView {
    type Inner = TreeView<()>;

    fn inner(&self) -> &TreeView<()> {
        &self.tree
    }

    fn inner_mut(&mut self) -> &mut TreeView<()> {
        &mut self.tree
    }

    fn name(&self) -> &'static str {
        "TreeView"
    }

    /// The tree's change is the described tree's: it adds no structure of its own.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.tree.take_structure_change()
    }
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

    /// Two people moving a node into each other's subtree at the same moment can leave links
    /// that close a loop. Every node is still held once: the one that comes last in the order
    /// does not follow its link and stands at the top level.
    #[test]
    fn parents_that_close_a_loop_keep_every_node_once() {
        let queue = crate::Intents::default();
        let layout = Layout::of(
            vec![
                node("a", Some("b"), false, &queue),
                node("b", Some("a"), true, &queue),
                node("c", Some("a"), false, &queue),
                node("itself", Some("itself"), false, &queue),
            ],
            Event::None,
        );
        assert_eq!(layout.ids.len(), 4, "every node is held once");
        assert_eq!(layout.parents.get("b"), Some(&None));
        assert_eq!(layout.parents.get("a"), Some(&Some("b".to_owned())));
        assert_eq!(layout.parents.get("c"), Some(&Some("a".to_owned())));
        assert_eq!(layout.parents.get("itself"), Some(&None));
        let root = layout.root(None);
        for key in ["a", "b", "c", "itself"] {
            let id = layout.ids[key];
            assert!(root.find(id).is_some(), "{key} is in the tree");
        }
    }

    /// Hiding the root drops the tree's own name and keeps every node at the top level, one of
    /// them as much as several: the heading above the tree names it once.
    #[test]
    fn hiding_the_root_keeps_every_top_level_node() {
        let queue = crate::Intents::default();
        let layout = Layout::of(
            vec![
                node("a", None, false, &queue),
                node("b", None, false, &queue),
            ],
            Event::None,
        );
        let name = "Subtasks".to_owned();
        let labels = |tree: &TreeView<()>| -> Vec<(String, usize)> {
            tree.visible_rows()
                .into_iter()
                .map(|(_, label, depth)| (label.to_owned(), depth))
                .collect()
        };
        let named = TreeView::new(layout.root(Some(&name)));
        assert_eq!(
            labels(&named),
            [("Subtasks".to_owned(), 0)],
            "the tree's name stands over its nodes when it is not hidden"
        );
        let hidden = TreeView::new(layout.root(Some(&name))).hide_root();
        assert_eq!(
            labels(&hidden),
            [("a".to_owned(), 0), ("b".to_owned(), 0)],
            "the node's rows are the nodes, not the tree's name"
        );
    }

    /// A move is reported by the node it lands at: the next sibling it goes before, the node
    /// it goes inside of, or the tree's own end.
    #[test]
    fn a_move_is_reported_by_the_node_it_lands_at() {
        let queue = crate::Intents::default();
        let layout = Layout::of(
            vec![
                node("a", None, true, &queue),
                node("b", Some("a"), false, &queue),
                node("c", None, false, &queue),
            ],
            Event::View(Intent::new(&queue, "tree", "drop")),
        );
        let id = |key: &str| layout.ids[key];
        let moved = |key: &str| Some(Value::Resource(key.to_owned()));

        assert!(layout.report(&ReorderDrop {
            source: id("c"),
            target: DropTarget::new(id("b"), DropPlacement::Before),
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
        assert!(layout.report(&ReorderDrop {
            source: id("c"),
            target: DropTarget::new(id("a"), DropPlacement::Inside),
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
        assert!(layout.report(&ReorderDrop {
            source: id("c"),
            target: DropTarget::new(id("c"), DropPlacement::Inside),
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
        assert!(layout.report(&ReorderDrop {
            source: id("b"),
            target: DropTarget::new(0, DropPlacement::After),
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
    /// row the app worded itself keeps its own.
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
        let layout = Layout::of(vec![said, child, worded], Event::None);
        let rows = layout.rows();
        assert_eq!(rows[&layout.ids["a"]].progress.as_deref(), Some("1/3"));
        assert_eq!(rows[&layout.ids["b"]].progress, None, "nothing says it");
        assert_eq!(
            rows[&layout.ids["c"]].progress.as_deref(),
            Some("2/5"),
            "the row's own wording stands"
        );
    }

    /// How far a branch got is said while it is folded; open, its own rows say it.
    #[test]
    fn progress_is_said_only_while_the_branch_is_folded() {
        let theme = crate::Theme::dark();
        let row = TreeViewRowProps {
            progress: Some("2/5".into()),
            trailing: Some("Fri".into()),
            checkbox: Some(true),
            ..TreeViewRowProps::default()
        };
        let info = |is_branch, expanded| TreeRowInfo {
            id: 1,
            depth: 1,
            is_branch,
            expanded,
            selected: false,
            current: false,
        };
        let folded = decoration(&row, &info(true, false), &theme);
        assert_eq!(folded.progress.as_deref(), Some("2/5"));
        assert_eq!(folded.trailing.as_deref(), Some("Fri"));
        assert_eq!(folded.checkbox, Some(true));
        assert_eq!(decoration(&row, &info(true, true), &theme).progress, None);
        assert_eq!(decoration(&row, &info(false, false), &theme).progress, None);
    }
}
