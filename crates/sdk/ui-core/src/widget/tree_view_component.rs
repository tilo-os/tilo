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

use crate::component::{Component, Described, Event, ResourceId, Value};
use crate::icons::Icon;
use crate::widget::Widget;
use crate::widget::tree_view::{
    TreeClickActivation, TreeNode, TreeRowDecoration, TreeRowTone, TreeView,
};

/// What one row of a tree shows besides its label: a leading glyph, a tone, trailing text and
/// what assistive technology says of it.
#[derive(Clone, Component, Debug, Default, PartialEq)]
#[component(name = "TreeViewRow", role = "Generic")]
pub struct TreeViewRowProps {
    /// A leading glyph: the kind of thing it is.
    pub icon: Option<Icon>,
    /// The tone its label reads in.
    pub tone: Option<TreeTone>,
    /// Trailing text: a count, a kind, a size.
    pub trailing: Option<String>,
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
#[derive(Clone, Component, Debug, Default)]
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
    /// Whether it is the selected node.
    pub selected: bool,
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
    /// The tree's one root node's label is its own name: the nodes under it form the top level.
    pub hide_root: bool,
    /// A press selects and activates in one, as a navigator sidebar does; a double click
    /// activates otherwise.
    pub single_click: bool,
    /// Moving through the rows with the keyboard selects them as it goes.
    pub selection_follows_focus: bool,
    /// Whether several nodes may be selected at once (Ctrl-click, Shift-click).
    pub multi_select: bool,
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
    /// A node was selected: carries the resource it shows.
    #[component(event(value = ResourceId))]
    pub select: Event,
    /// A node was activated (a double click, Enter, an assistive technology): carries the
    /// resource it shows.
    #[component(event(value = ResourceId))]
    pub activate: Event,
}

/// A described tree: its tree, and the resource and the widget id of each node, by the node's
/// key.
pub struct DescribedTreeView {
    tree: TreeView<()>,
    items: HashMap<String, ResourceId>,
    ids: HashMap<String, u64>,
    /// What the tree was made with, its state aside.
    face: Face,
}

/// What decides the tree, its nodes' own changes besides: the same again is taken in place.
#[derive(Clone, Debug, PartialEq)]
struct Face {
    name: Option<String>,
    fill: bool,
    hide_root: bool,
    single_click: bool,
    selection_follows_focus: bool,
    multi_select: bool,
    /// Each node's resource, label, parent and whether it is a branch: a change of these, or of a
    /// node's coming in, builds the tree again.
    nodes: Vec<(String, String, Option<String>, bool)>,
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
        self.ids.get(key).copied()
    }

    /// The resource the node with `key` stands for.
    #[must_use]
    pub fn item_of(&self, key: &str) -> Option<&ResourceId> {
        self.items.get(key)
    }
}

fn decoration(row: &TreeViewRowProps, theme: &crate::theme::Theme) -> TreeRowDecoration {
    let mut decoration = TreeRowDecoration::new();
    if let Some(icon) = row.icon {
        decoration = decoration.icon(icon, theme.text_primary);
    }
    decoration = decoration.tone(match row.tone.unwrap_or_default() {
        TreeTone::Primary => TreeRowTone::Primary,
        TreeTone::Secondary => TreeRowTone::Secondary,
    });
    if let Some(text) = &row.trailing {
        decoration = decoration.trailing(text.clone());
    }
    if let Some(text) = &row.description {
        decoration = decoration.description(text.clone());
    }
    decoration
}

/// The hierarchy a tree's nodes make: the roots, each node's children by key, and each node's
/// widget id.
struct Nodes {
    roots: Vec<TreeViewNodeProps>,
    children: HashMap<String, Vec<TreeViewNodeProps>>,
    ids: HashMap<String, u64>,
}

impl Nodes {
    fn of(nodes: &[TreeViewNodeProps]) -> Self {
        let mut ids: HashMap<String, u64> = HashMap::new();
        for node in nodes {
            if !ids.contains_key(&node.item.0) {
                let id = u64::try_from(ids.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(1);
                ids.insert(node.item.0.clone(), id);
            }
        }
        let keys: HashSet<&str> = nodes.iter().map(|node| node.item.0.as_str()).collect();
        let mut children: HashMap<String, Vec<TreeViewNodeProps>> = HashMap::new();
        let mut roots = Vec::new();
        for node in nodes {
            match node.parent.as_ref().map(|parent| parent.0.as_str()) {
                Some(parent) if keys.contains(parent) && parent != node.item.0 => {
                    children
                        .entry(parent.to_owned())
                        .or_default()
                        .push(node.clone());
                }
                _ => roots.push(node.clone()),
            }
        }
        Self {
            roots,
            children,
            ids,
        }
    }

    /// The node part as one of the widget's own nodes, its children under it.
    fn tree_node(&self, node: &TreeViewNodeProps) -> TreeNode<()> {
        let id = self.ids.get(&node.item.0).copied().unwrap_or(0);
        let children: Vec<TreeNode<()>> = self
            .children
            .get(&node.item.0)
            .map(|children| children.iter().map(|child| self.tree_node(child)).collect())
            .unwrap_or_default();
        if node.branch || !children.is_empty() {
            TreeNode::branch(id, node.label.clone(), (), children)
        } else {
            TreeNode::leaf(id, node.label.clone(), ())
        }
    }

    /// The tree's root: the one root node's children, its label the tree's name, when there is
    /// one root and the view hides it; a node of the tree's own name over the roots otherwise.
    fn root(&self, name: Option<&String>, hide_root: bool) -> TreeNode<()> {
        if hide_root && self.roots.len() == 1 {
            let root = &self.roots[0];
            let children: Vec<TreeNode<()>> = self
                .children
                .get(&root.item.0)
                .map(|children| children.iter().map(|child| self.tree_node(child)).collect())
                .unwrap_or_default();
            return TreeNode::branch(
                self.ids.get(&root.item.0).copied().unwrap_or(0),
                root.label.clone(),
                (),
                children,
            );
        }
        TreeNode::branch(
            0,
            name.cloned().unwrap_or_default(),
            (),
            self.roots.iter().map(|node| self.tree_node(node)).collect(),
        )
    }
}

/// The key of the node with the widget id `id`, when it has one.
fn key_of(ids: &HashMap<String, u64>, id: crate::widget::tree_view::NodeId) -> Option<String> {
    ids.iter()
        .find(|(_, held)| **held == id)
        .map(|(key, _)| key.clone())
}

impl Described for DescribedTreeView {
    type Props = TreeViewProps;

    fn from_props(key: &str, props: TreeViewProps) -> Self {
        let TreeViewProps {
            name,
            fill,
            hide_root,
            single_click,
            selection_follows_focus,
            multi_select,
            nodes,
            expanded,
            selection,
            select,
            activate,
        } = props;
        let face = Face {
            name: name.clone(),
            fill,
            hide_root,
            single_click,
            selection_follows_focus,
            multi_select,
            nodes: nodes
                .iter()
                .map(|node| {
                    (
                        node.item.0.clone(),
                        node.label.clone(),
                        node.parent.clone().map(|parent| parent.0),
                        node.branch,
                    )
                })
                .collect(),
        };
        let hierarchy = Nodes::of(&nodes);
        let root = hierarchy.root(name.as_ref(), hide_root);
        let mut items: HashMap<String, ResourceId> = HashMap::new();
        let mut rows: HashMap<String, TreeViewRowProps> = HashMap::new();
        for node in nodes {
            let key = node.item.0.clone();
            if let Some(row) = node.row {
                rows.insert(key.clone(), row);
            }
            items.insert(key, node.item);
        }
        let ids = hierarchy.ids.clone();
        let mut tree = TreeView::new(root).with_id(key);
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
        let rows_by_id: HashMap<u64, TreeViewRowProps> = rows
            .into_iter()
            .filter_map(|(key, row)| ids.get(&key).map(|id| (*id, row)))
            .collect();
        tree = tree.with_row_decoration(
            move |_: &TreeNode<()>,
                  info: crate::widget::TreeRowInfo,
                  theme: &crate::theme::Theme| {
                rows_by_id
                    .get(&info.id)
                    .map_or_else(TreeRowDecoration::new, |row| decoration(row, theme))
            },
        );
        if !select.is_none() {
            let ids = ids.clone();
            let items = items.clone();
            tree = tree.on_select(move |id: crate::widget::tree_view::NodeId| {
                if let Some(key) = key_of(&ids, id)
                    && let Some(item) = items.get(&key)
                {
                    select.fire(Some(Value::Resource(item.0.clone())));
                }
            });
        }
        if !activate.is_none() {
            let ids = ids.clone();
            let items = items.clone();
            tree = tree.on_activate(move |id: crate::widget::tree_view::NodeId| {
                if let Some(key) = key_of(&ids, id)
                    && let Some(item) = items.get(&key)
                {
                    activate.fire(Some(Value::Resource(item.0.clone())));
                }
            });
        }
        if let Some(open) = &expanded {
            let wanted: Vec<u64> = open
                .iter()
                .filter_map(|key| ids.get(&key.0).copied())
                .collect();
            tree = tree.with_expanded(wanted);
        }
        if let Some(selected) = &selection {
            let mut chosen = crate::selection::KeySelection::<u64>::new();
            for key in selected {
                if let Some(id) = ids.get(&key.0).copied() {
                    chosen.toggle(id);
                }
            }
            tree = tree.with_selection(chosen);
        }
        Self {
            tree,
            items,
            ids,
            face,
        }
    }

    /// Its expansion and selection are taken in place, so a branch a person folded stays folded
    /// while the selection moves. Any other change of the nodes, or of the tree itself, builds
    /// it again.
    fn set_props(&mut self, props: TreeViewProps) -> bool {
        let TreeViewProps {
            name,
            fill,
            hide_root,
            single_click,
            selection_follows_focus,
            multi_select,
            nodes,
            expanded,
            selection,
            select: _,
            activate: _,
        } = props;
        let face = Face {
            name,
            fill,
            hide_root,
            single_click,
            selection_follows_focus,
            multi_select,
            nodes: nodes
                .iter()
                .map(|node| {
                    (
                        node.item.0.clone(),
                        node.label.clone(),
                        node.parent.clone().map(|parent| parent.0),
                        node.branch,
                    )
                })
                .collect(),
        };
        if face != self.face {
            return false;
        }
        if let Some(open) = &expanded {
            let wanted: Vec<u64> = open
                .iter()
                .filter_map(|key| self.ids.get(&key.0).copied())
                .collect();
            self.tree.restore_expanded(wanted);
        }
        if let Some(selected) = &selection {
            let mut chosen = crate::selection::KeySelection::<u64>::new();
            for key in selected {
                if let Some(id) = self.ids.get(&key.0).copied() {
                    chosen.toggle(id);
                }
            }
            *self.tree.selection_mut() = chosen;
        }
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
