//! `TreeView` — hierarchical list. One row per visible node, indented by
//! depth; branch nodes carry an expand/collapse glyph. Selection and focus
//! are tracked separately so arrow-key navigation can move the focus ring
//! without committing a selection until the user explicitly chooses.
//!
//! Expand/select/focus state lives on the widget; `on_select` is how the
//! caller persists a choice into its own (e.g. CRDT-backed) storage.

use crate::num::f32_of_f64;
use crate::num::f64_of_usize;
use crate::time::{Duration, Instant};
use crate::widget::{AppModel, ModelPath};
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use tilo_sort::CompactSort as _;

use taffy::style_helpers::{length, percent};
use taffy::{Dimension, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use vello::peniko::Color;
use winit::event::{
    ElementState, KeyEvent, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};
use winit::keyboard::{Key, NamedKey};

use crate::collection_transition::{KeyedRowSnapshot, KeyedTransitionSet, RowGeometry};
use crate::color_compat::ColorCompat;
use crate::icons::Icon;
use crate::presence::{PresenceCtx, PresenceKey};
use crate::reorder::{DropCandidate, DropPlacement, DropTarget, ReorderController, ReorderDrop};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::virtual_viewport::{
    DragScroller, ScrollAlignment, ScrollMomentum, SparseAnchor, SparseVirtualViewport, WheelStep,
};
use crate::widget::EventCtx;
use crate::widget::context_menu::{ContextMenu, MenuItem};
use crate::widget::data_table::{
    RowType, centered_text_top, row_estimate, text_band, type_line_height,
};
use crate::widget::{Element, TaffyTreeExt, Widget};

mod access_rows;
mod affordances;
mod editing;
#[cfg(test)]
mod editing_tests;
mod nav;

pub use affordances::{TreeHeadingAction, TreeRowAction};
pub use editing::TreeMove;
pub(crate) use nav::{NavItemSpec, NavPresentation, NavRowSpec, NavStatus};

pub type NodeId = u64;

/// Stable address of one logical child slot in a paged model. Slots remain
/// present while their data transitions from loading to ready or error.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TreeSlot {
    pub parent: NodeId,
    pub index: usize,
}

/// State returned for a logical child slot. A model can publish its total
/// child count before all child ids are resident, allowing the tree to retain
/// correct scrollbar geometry while requesting only visible pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeChildState<'a> {
    Ready(NodeId),
    Loading,
    Error { message: Option<&'a str> },
}

/// Non-resident row state passed to [`TreeView::with_slot_delegate`].
///
/// The slot retains a stable identity while this state changes, so a delegate
/// can render localized progress, retry, or diagnostic controls without
/// inventing a placeholder node id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeSlotState<'a> {
    Loading,
    Error { message: Option<&'a str> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeLoadReason {
    Visible,
    Retry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeLoadRequest {
    pub slot: TreeSlot,
    pub reason: TreeLoadReason,
}

/// Stable identity of either a resident model node or an unresolved child
/// slot. Selection remains node-only, while focus and viewport anchoring can
/// safely include placeholders.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TreeRowKey {
    Node(NodeId),
    Slot(TreeSlot),
}

/// How a pointer press activates a row (see [`TreeView::on_activate`]).
/// Keyboard `Enter` and the accessibility `Activate` verb activate the
/// focused row in either mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TreeClickActivation {
    /// A single click selects; a double click activates. Suits trees where
    /// activation is heavy or leaves the tree (a file manager opening a
    /// document in another app).
    #[default]
    DoubleClick,
    /// A single click selects and activates, as a navigator sidebar does
    /// (click a file to open it). Arrow keys still only move focus and
    /// selection, so browsing never opens every row it passes.
    SingleClick,
}

/// What a [`TreeView::with_row_decoration`] callback is told about the row
/// it decorates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeRowInfo {
    pub id: NodeId,
    pub depth: usize,
    pub is_branch: bool,
    pub expanded: bool,
    pub selected: bool,
    /// The row is the host's current item (see [`TreeView::set_current`]).
    pub current: bool,
}

/// A leading glyph for a decorated row. Colours are resolved by the caller
/// from the `Theme` handed to the decoration callback, so they track the
/// active theme.
#[derive(Clone, Debug)]
pub enum TreeRowIcon {
    /// A toolkit icon stroked in `color`.
    Icon { icon: Icon, color: Color },
    /// A text glyph (a symbol or a short code) painted in `color`.
    Glyph { text: String, color: Color },
    /// A short code on a filled chip; the ink is derived from the fill so it
    /// stays legible on any colour.
    Badge { text: String, fill: Color },
}

/// The label colour role of a decorated row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TreeRowTone {
    /// Body text colour.
    #[default]
    Primary,
    /// The theme's secondary text colour, for dense or subordinate rows.
    Secondary,
}

/// Per-row presentation painted by the tree itself: a leading icon, a label
/// tone, trailing secondary text (a count, a type) and an accessible
/// description. Unlike [`TreeView::with_row_delegate`] nothing is mounted, so
/// decorations are recomputed every frame at no retained cost; disclosure,
/// selection, hover, the current row, focus and semantics stay owned by the
/// tree.
#[derive(Clone, Debug, Default)]
pub struct TreeRowDecoration {
    pub icon: Option<TreeRowIcon>,
    pub tone: TreeRowTone,
    /// Right-aligned secondary text; the label elides before it.
    pub trailing: Option<String>,
    /// Spoken after the label (e.g. "View", "Folder").
    pub description: Option<String>,
    /// Collaborators present at this location glow the row in their colour
    /// (the peer's "indicative mist"), so a tree shows where everyone is.
    pub presence: Option<PresenceKey>,
}

impl TreeRowDecoration {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn icon(mut self, icon: Icon, color: Color) -> Self {
        self.icon = Some(TreeRowIcon::Icon { icon, color });
        self
    }

    #[must_use]
    pub fn glyph(mut self, text: impl Into<String>, color: Color) -> Self {
        self.icon = Some(TreeRowIcon::Glyph {
            text: text.into(),
            color,
        });
        self
    }

    #[must_use]
    pub fn badge(mut self, text: impl Into<String>, fill: Color) -> Self {
        self.icon = Some(TreeRowIcon::Badge {
            text: text.into(),
            fill,
        });
        self
    }

    #[must_use]
    pub fn tone(mut self, tone: TreeRowTone) -> Self {
        self.tone = tone;
        self
    }

    #[must_use]
    pub fn trailing(mut self, text: impl Into<String>) -> Self {
        self.trailing = Some(text.into());
        self
    }

    #[must_use]
    pub fn description(mut self, text: impl Into<String>) -> Self {
        self.description = Some(text.into());
        self
    }

    #[must_use]
    pub fn presence(mut self, key: PresenceKey) -> Self {
        self.presence = Some(key);
        self
    }
}

/// Source-agnostic hierarchy queried by stable id. Implementations may be
/// backed by a database, an async cache, a CRDT, or the owned [`TreeNode`]
/// adapter. The view asks only for expanded children and visible labels.
/// IDs must be unique, parent/child queries must be inverse, and the reachable
/// hierarchy must be acyclic.
pub trait TreeModel {
    fn root_id(&self) -> NodeId;
    fn contains(&self, id: NodeId) -> bool;
    fn label(&self, id: NodeId) -> Option<&str>;
    fn is_branch(&self, id: NodeId) -> bool;
    /// Whether a drop or an indent may make `id` a parent. Defaults to
    /// [`Self::is_branch`]; a hierarchy where any node can hold children (an
    /// outline, a notebook) answers `true` for leaves too, while
    /// [`Self::is_branch`] keeps deciding which rows show a disclosure.
    fn accepts_children(&self, id: NodeId) -> bool {
        self.is_branch(id)
    }
    /// A durable identity for `id` in the semantic tree (`"notes:page:<uuid>"`),
    /// for models whose node ids are only stable within a session. Assistive
    /// technology and automation then address the row by it across launches.
    fn semantic_key(&self, _id: NodeId) -> Option<String> {
        None
    }
    /// Whether `id` can be chosen. A row that cannot (a section header in a
    /// navigation list) still takes focus and expands, but is never
    /// selected or activated.
    fn is_selectable(&self, _id: NodeId) -> bool {
        true
    }
    /// Whether `id`'s label can be edited in place (see
    /// [`TreeView::on_rename`]).
    fn is_renamable(&self, _id: NodeId) -> bool {
        true
    }
    /// Whether `id`'s row shows the tree's row actions on hover and focus.
    fn shows_row_actions(&self, _id: NodeId) -> bool {
        true
    }
    /// Whether an expanded branch may be collapsed. A heading whose rows
    /// are always shown answers `false`.
    fn can_collapse(&self, _id: NodeId) -> bool {
        true
    }
    /// Parent of `id`; `None` only for the root or an unknown id.
    fn parent_id(&self, id: NodeId) -> Option<NodeId>;
    /// Position of a direct child. Large/paged models should answer this from
    /// an index rather than scanning siblings.
    fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize>;
    fn child_count(&self, parent: NodeId) -> usize;
    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId>;

    /// Resolve a logical child slot. Paged models override this to return
    /// [`TreeChildState::Loading`] or [`TreeChildState::Error`] for
    /// non-resident rows; existing fully-resident models need only implement
    /// [`Self::child_id`].
    fn child_state(&self, parent: NodeId, index: usize) -> TreeChildState<'_> {
        self.child_id(parent, index).map_or(
            TreeChildState::Error {
                message: Some("Row unavailable"),
            },
            TreeChildState::Ready,
        )
    }

    /// Start or deduplicate work for one visible/retried child slot. Return
    /// whether the model accepted the request. Model-owned asynchronous
    /// sources can complete through [`Self::poll_changes`]; external owners
    /// may instead use [`TreeView::on_load_request`].
    fn request_load(&mut self, _request: TreeLoadRequest) -> bool {
        false
    }

    /// Drain model-owned structural, row-data, or slot-state changes.
    fn poll_changes(&mut self) -> Vec<TreeModelChange> {
        Vec::new()
    }

    /// Keep scheduling frames only for sources that cannot wake the app when
    /// a batch completes. Prefer [`crate::LatestRequest`] for idle workers.
    fn needs_poll(&self) -> bool {
        false
    }
}

/// A granular notification from an external tree model. Structural and slot
/// identity changes reconcile the expanded-order index once per batch; label
/// changes preserve geometry and query fresh data on the next frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeModelChange {
    Reset,
    RowsInserted {
        parent: NodeId,
        index: usize,
        count: usize,
    },
    RowsRemoved {
        parent: NodeId,
        index: usize,
        count: usize,
    },
    RowsMoved {
        parent: NodeId,
        from: usize,
        to: usize,
        count: usize,
    },
    RowsChanged {
        ids: Vec<NodeId>,
    },
    /// One or more paged slots changed between loading, ready, and error.
    SlotsChanged {
        slots: Vec<TreeSlot>,
    },
}

pub struct TreeNode<T> {
    pub id: NodeId,
    pub label: String,
    pub data: T,
    pub children: Vec<TreeNode<T>>,
    branch: bool,
}

impl<T> TreeNode<T> {
    pub fn leaf(id: NodeId, label: impl Into<String>, data: T) -> Self {
        Self {
            id,
            label: label.into(),
            data,
            children: Vec::new(),
            branch: false,
        }
    }

    /// A branch with zero children still renders the disclosure glyph,
    /// so callers can populate it (e.g. an empty folder) later.
    pub fn branch(
        id: NodeId,
        label: impl Into<String>,
        data: T,
        children: Vec<TreeNode<T>>,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            data,
            children,
            branch: true,
        }
    }

    #[must_use]
    pub fn is_branch(&self) -> bool {
        self.branch
    }

    #[must_use]
    pub fn find(&self, id: NodeId) -> Option<&Self> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(id))
    }

    pub fn find_mut(&mut self, id: NodeId) -> Option<&mut Self> {
        if self.id == id {
            return Some(self);
        }
        self.children
            .iter_mut()
            .find_map(|child| child.find_mut(id))
    }

    /// Whether an owned node can be moved to this stable drop target.
    ///
    /// Root moves, self-drops, drops inside leaves, and moves that would put a
    /// node below one of its own descendants are rejected. Apps with further
    /// domain constraints can combine this with [`TreeView::with_reorder`].
    #[must_use]
    pub fn can_reorder(&self, source: NodeId, target: &DropTarget<NodeId>) -> bool {
        if source == self.id || source == target.key {
            return false;
        }
        let (Some(source_node), Some(target_node)) = (self.find(source), self.find(target.key))
        else {
            return false;
        };
        if matches!(target.placement, DropPlacement::Inside) && !target_node.is_branch() {
            return false;
        }
        let destination_parent = match target.placement {
            DropPlacement::Inside => Some(target.key),
            DropPlacement::Before | DropPlacement::After => self.parent_id(target.key),
        };
        destination_parent.is_some_and(|parent| source_node.find(parent).is_none())
    }

    /// Apply a validated before/inside/after move to this owned hierarchy.
    ///
    /// The complete source subtree is preserved. Indices are resolved again
    /// after detaching the source, so sibling moves cannot suffer the usual
    /// off-by-one error. Moving a node to its current position returns
    /// `false` without mutating the hierarchy.
    pub fn reorder(&mut self, dropped: &ReorderDrop<NodeId>) -> bool {
        if !self.can_reorder(dropped.source, &dropped.target)
            || self.reorder_is_noop(dropped.source, &dropped.target)
        {
            return false;
        }
        let Some(moved) = Self::take_descendant(self, dropped.source) else {
            return false;
        };
        match dropped.target.placement {
            DropPlacement::Inside => {
                let Some(parent) = self.find_mut(dropped.target.key) else {
                    return false;
                };
                parent.children.push(moved);
            }
            DropPlacement::Before | DropPlacement::After => {
                let Some(parent_id) = self.parent_id(dropped.target.key) else {
                    return false;
                };
                let Some(parent) = self.find_mut(parent_id) else {
                    return false;
                };
                let Some(target_index) = parent
                    .children
                    .iter()
                    .position(|child| child.id == dropped.target.key)
                else {
                    return false;
                };
                let insert_at = if matches!(dropped.target.placement, DropPlacement::After) {
                    target_index.saturating_add(1)
                } else {
                    target_index
                };
                parent.children.insert(insert_at, moved);
            }
        }
        true
    }

    fn reorder_is_noop(&self, source: NodeId, target: &DropTarget<NodeId>) -> bool {
        let Some(source_parent) = self.parent_id(source) else {
            return true;
        };
        let Some(source_index) = self.child_index(source_parent, source) else {
            return true;
        };
        match target.placement {
            DropPlacement::Inside => {
                source_parent == target.key
                    && source_index.saturating_add(1) == self.child_count(target.key)
            }
            DropPlacement::Before | DropPlacement::After => {
                let Some(target_parent) = self.parent_id(target.key) else {
                    return true;
                };
                let Some(target_index) = self.child_index(target_parent, target.key) else {
                    return true;
                };
                source_parent == target_parent
                    && match target.placement {
                        DropPlacement::Before => source_index.saturating_add(1) == target_index,
                        DropPlacement::After => target_index.saturating_add(1) == source_index,
                        DropPlacement::Inside => false,
                    }
            }
        }
    }

    fn take_descendant(node: &mut Self, id: NodeId) -> Option<Self> {
        if let Some(index) = node.children.iter().position(|child| child.id == id) {
            return Some(node.children.remove(index));
        }
        node.children
            .iter_mut()
            .find_map(|child| Self::take_descendant(child, id))
    }
}

impl<T> TreeModel for TreeNode<T> {
    fn root_id(&self) -> NodeId {
        self.id
    }

    fn contains(&self, id: NodeId) -> bool {
        self.find(id).is_some()
    }

    fn label(&self, id: NodeId) -> Option<&str> {
        self.find(id).map(|node| node.label.as_str())
    }

    fn is_branch(&self, id: NodeId) -> bool {
        self.find(id).is_some_and(TreeNode::is_branch)
    }

    fn parent_id(&self, id: NodeId) -> Option<NodeId> {
        fn find_parent<T>(node: &TreeNode<T>, id: NodeId) -> Option<NodeId> {
            if node.children.iter().any(|child| child.id == id) {
                return Some(node.id);
            }
            node.children
                .iter()
                .find_map(|child| find_parent(child, id))
        }
        find_parent(self, id)
    }

    fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
        self.find(parent)?
            .children
            .iter()
            .position(|node| node.id == child)
    }

    fn child_count(&self, parent: NodeId) -> usize {
        self.find(parent).map_or(0, |node| node.children.len())
    }

    fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
        self.find(parent)?.children.get(index).map(|node| node.id)
    }
}

#[derive(Clone, Debug)]
struct ExpandedChild {
    position: usize,
    node: Box<ExpandedIndexNode>,
}

#[derive(Clone, Debug)]
struct ExpandedIndexNode {
    id: NodeId,
    direct_children: usize,
    visible_len: usize,
    expanded_children: Vec<ExpandedChild>,
}

/// Sparse overlay of expanded nodes. A branch with 100M collapsed children
/// occupies one node plus a count; only expanded descendants add entries.
#[derive(Clone, Debug)]
struct SparseTreeIndex {
    root: ExpandedIndexNode,
    /// The root is always expanded and not itself a row: its children are
    /// the top level (depth 0). See [`TreeView::hide_root`].
    root_hidden: bool,
}

impl SparseTreeIndex {
    fn build<M: TreeModel>(model: &M, expanded: &HashSet<NodeId>, root_hidden: bool) -> Self {
        let root_id = model.root_id();
        let mut grouped = HashMap::<NodeId, Vec<(usize, NodeId)>>::new();
        for id in expanded {
            if *id == root_id || !model.is_branch(*id) {
                continue;
            }
            let Some(parent) = model.parent_id(*id) else {
                continue;
            };
            let Some(position) = model.child_index(parent, *id) else {
                continue;
            };
            if model.child_id(parent, position) == Some(*id) {
                grouped.entry(parent).or_default().push((position, *id));
            }
        }
        for children in grouped.values_mut() {
            children.compact_sort_by_key(|(position, _)| *position);
            children.dedup_by_key(|(position, _)| *position);
        }
        let root = if root_hidden && !expanded.contains(&root_id) {
            let mut with_root = expanded.clone();
            with_root.insert(root_id);
            Self::build_node(model, &with_root, &mut grouped, root_id)
        } else {
            Self::build_node(model, expanded, &mut grouped, root_id)
        };
        Self { root, root_hidden }
    }

    /// Rows the hidden root would occupy ahead of every visible row.
    fn offset(&self) -> usize {
        usize::from(self.root_hidden)
    }

    fn build_node<M: TreeModel>(
        model: &M,
        expanded: &HashSet<NodeId>,
        grouped: &mut HashMap<NodeId, Vec<(usize, NodeId)>>,
        id: NodeId,
    ) -> ExpandedIndexNode {
        if !expanded.contains(&id) || !model.is_branch(id) {
            return ExpandedIndexNode {
                id,
                direct_children: 0,
                visible_len: 1,
                expanded_children: Vec::new(),
            };
        }
        let direct_children = model.child_count(id);
        let expanded_children = grouped
            .remove(&id)
            .unwrap_or_default()
            .into_iter()
            .filter(|(position, _)| *position < direct_children)
            .map(|(position, child)| ExpandedChild {
                position,
                node: Box::new(Self::build_node(model, expanded, grouped, child)),
            })
            .collect::<Vec<_>>();
        let extra = expanded_children.iter().fold(0_usize, |total, child| {
            total.saturating_add(child.node.visible_len.saturating_sub(1))
        });
        ExpandedIndexNode {
            id,
            direct_children,
            visible_len: 1_usize
                .saturating_add(direct_children)
                .saturating_add(extra),
            expanded_children,
        }
    }

    fn len(&self) -> usize {
        self.root.visible_len.saturating_sub(self.offset())
    }

    fn row_at<M: TreeModel>(&self, model: &M, index: usize) -> Option<(TreeRowKey, usize)> {
        let offset = self.offset();
        let (key, depth) = Self::row_in_node(model, &self.root, index.checked_add(offset)?, 0)?;
        Some((key, depth.saturating_sub(offset)))
    }

    fn row_in_node<M: TreeModel>(
        model: &M,
        node: &ExpandedIndexNode,
        index: usize,
        depth: usize,
    ) -> Option<(TreeRowKey, usize)> {
        if index == 0 {
            return Some((TreeRowKey::Node(node.id), depth));
        }
        if index >= node.visible_len {
            return None;
        }
        let mut remaining = index.saturating_sub(1);
        let mut child_cursor = 0_usize;
        for expanded in &node.expanded_children {
            let plain = expanded.position.saturating_sub(child_cursor);
            if remaining < plain {
                let child_index = child_cursor.saturating_add(remaining);
                let key = match model.child_state(node.id, child_index) {
                    TreeChildState::Ready(id) => TreeRowKey::Node(id),
                    TreeChildState::Loading | TreeChildState::Error { .. } => {
                        TreeRowKey::Slot(TreeSlot {
                            parent: node.id,
                            index: child_index,
                        })
                    }
                };
                return Some((key, depth.saturating_add(1)));
            }
            remaining = remaining.saturating_sub(plain);
            if remaining < expanded.node.visible_len {
                return Self::row_in_node(
                    model,
                    &expanded.node,
                    remaining,
                    depth.saturating_add(1),
                );
            }
            remaining = remaining.saturating_sub(expanded.node.visible_len);
            child_cursor = expanded.position.saturating_add(1);
        }
        let child = child_cursor.saturating_add(remaining);
        (child < node.direct_children).then(|| {
            let key = match model.child_state(node.id, child) {
                TreeChildState::Ready(id) => TreeRowKey::Node(id),
                TreeChildState::Loading | TreeChildState::Error { .. } => {
                    TreeRowKey::Slot(TreeSlot {
                        parent: node.id,
                        index: child,
                    })
                }
            };
            (key, depth.saturating_add(1))
        })
    }

    fn index_of<M: TreeModel>(&self, model: &M, id: NodeId) -> Option<usize> {
        self.raw_index_of(model, id)?.checked_sub(self.offset())
    }

    /// Position counting the root as row 0 whether or not it is shown.
    fn raw_index_of<M: TreeModel>(&self, model: &M, id: NodeId) -> Option<usize> {
        if id == self.root.id {
            return Some(0);
        }
        let mut reversed = vec![id];
        let mut seen = HashSet::from([id]);
        let mut cursor = id;
        while cursor != self.root.id {
            let parent = model.parent_id(cursor)?;
            if !seen.insert(parent) {
                return None;
            }
            reversed.push(parent);
            cursor = parent;
        }
        reversed.reverse();

        let mut index = 0_usize;
        let mut overlay = &self.root;
        for child in reversed.into_iter().skip(1) {
            let position = model.child_index(overlay.id, child)?;
            if position >= overlay.direct_children {
                return None;
            }
            let extra_before = overlay
                .expanded_children
                .iter()
                .take_while(|expanded| expanded.position < position)
                .fold(0_usize, |total, expanded| {
                    total.saturating_add(expanded.node.visible_len.saturating_sub(1))
                });
            index = index
                .saturating_add(1)
                .saturating_add(position)
                .saturating_add(extra_before);
            if child != id {
                overlay = overlay
                    .expanded_children
                    .iter()
                    .find(|expanded| expanded.node.id == child)?
                    .node
                    .as_ref();
            }
        }
        (index < self.root.visible_len).then_some(index)
    }

    fn index_of_key<M: TreeModel>(&self, model: &M, key: TreeRowKey) -> Option<usize> {
        match key {
            TreeRowKey::Node(id) => self.index_of(model, id),
            TreeRowKey::Slot(slot) => {
                if matches!(
                    model.child_state(slot.parent, slot.index),
                    TreeChildState::Ready(_)
                ) {
                    return None;
                }
                let parent_index = self.raw_index_of(model, slot.parent)?;
                let overlay = Self::find_overlay(&self.root, slot.parent)?;
                if slot.index >= overlay.direct_children {
                    return None;
                }
                let extra_before = overlay
                    .expanded_children
                    .iter()
                    .take_while(|child| child.position < slot.index)
                    .fold(0_usize, |total, child| {
                        total.saturating_add(child.node.visible_len.saturating_sub(1))
                    });
                parent_index
                    .saturating_add(1)
                    .saturating_add(slot.index)
                    .saturating_add(extra_before)
                    .checked_sub(self.offset())
            }
        }
    }

    fn find_overlay(node: &ExpandedIndexNode, id: NodeId) -> Option<&ExpandedIndexNode> {
        if node.id == id {
            return Some(node);
        }
        node.expanded_children
            .iter()
            .find_map(|child| Self::find_overlay(&child.node, id))
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "independent focus, selection, and retained-delegate invalidation states"
)]
pub struct TreeView<T, M = TreeNode<T>> {
    test_id: Option<String>,
    model: M,
    data: PhantomData<fn() -> T>,
    expanded: HashSet<NodeId>,
    selection: crate::selection::KeySelection<NodeId>,
    multi_select: bool,
    focused: Option<TreeRowKey>,
    /// What has been typed into the focused list: typing a name's first
    /// letters moves to it ([`crate::type_ahead`]).
    typing: crate::type_ahead::TypeAhead,
    /// Current exported button target, distinct from the row owning it.
    focused_affordance: Option<String>,
    affordance_focus_valid: bool,
    on_select: Option<Box<dyn FnMut(NodeId)>>,
    on_expand: Option<Box<dyn FnMut(NodeId, bool)>>,
    on_load_request: Option<Box<dyn FnMut(TreeLoadRequest)>>,
    on_activate: Option<Box<dyn FnMut(NodeId)>>,
    click_activation: TreeClickActivation,
    /// Last single-click row and time, for double-click activation.
    last_click: Option<(NodeId, Instant)>,
    selection_follows_focus: bool,
    row_decoration: Option<TreeRowDecorator<M>>,
    /// The host's current item (an open file, the table on screen).
    current: Option<NodeId>,
    /// Section heading painted above the rows; also the default accessible
    /// name.
    heading: Option<String>,
    /// Height of the heading band, resolved from the theme at layout time.
    heading_h: f64,
    /// Surface the tree is painted on; `None` is `surfaces.base`.
    ground: Option<fn(&Theme) -> Color>,
    /// Take the height the parent allots and scroll internally.
    fill: bool,
    hovered: Option<TreeRowKey>,
    /// Accessible descriptions the row decoration gave the rows painted last
    /// frame; semantics read them without re-running the decoration.
    row_descriptions: HashMap<NodeId, String>,
    reorder: ReorderController<NodeId>,
    reorder_validator: Option<TreeReorderValidator<M>>,
    reorder_commit: Option<TreeReorderCommit<M>>,
    drag_scroll: DragScroller,
    momentum: ScrollMomentum,
    last_tick: Option<Instant>,
    transition_snapshot: Vec<KeyedRowSnapshot<TreeRowKey, TreeTransitionPayload>>,
    transitions: KeyedTransitionSet<TreeRowKey, TreeTransitionPayload>,
    /// The motion its row changes play (`motion::ListChange`).
    transition_motion: crate::tokens::EffectiveMotion,
    reduce_motion: bool,
    /// Disclosure column, resolved from the theme's label line at layout
    /// time so the glyph never overlaps the label at larger text scales
    /// (`DISCLOSURE_WIDTH` at the default scale).
    disclosure_w: f64,
    /// The estimated row height the host authored through `set_viewport`,
    /// if any. Honoured as written at the default text scale and raised only
    /// once the label line itself would not fit; the unauthored default
    /// follows the type with its 1× padding.
    authored_row_height: Option<f64>,
    /// The label type seen at the last layout; `None` until then, when the
    /// estimate is exactly the authored or default height.
    row_type: Option<RowType>,
    /// Indent per depth level, the theme's `s4` (16 px at 1×). Paint and
    /// hit-testing read the same value so the disclosure column a click
    /// lands in is the one that was drawn.
    indent_px: f64,
    requested_slots: HashSet<TreeSlot>,
    row_delegate: Option<TreeRowDelegate<M>>,
    slot_delegate: Option<TreeSlotDelegate<M>>,
    mounted_rows: Vec<MountedTreeRow>,
    delegate_structure_dirty: bool,
    delegate_layout_dirty: bool,
    accessible_name: Option<String>,
    rect: Rect,
    is_focused: bool,
    last_cursor: Option<Point>,
    /// The overlay bar's idle fade: shown while the rows scroll, then gone,
    /// like every other scroll bar. Painting notices the offset move.
    bar_fade: std::cell::Cell<crate::widget::scrollbar::BarFade>,
    /// The offset the bar was last painted at.
    bar_offset: std::cell::Cell<Option<f64>>,
    /// Something keeps the bar shown (the pointer at it, reduced motion).
    bar_held: std::cell::Cell<bool>,
    /// Sparse expanded-order overlay; memory is proportional to expanded
    /// branches rather than their total visible descendants.
    index: SparseTreeIndex,
    /// Stable node-or-slot keys connect expansion reconciliation, variable row
    /// measurement, painting, hit testing, keyboard reveal, and semantics.
    viewport: SparseVirtualViewport<TreeRowKey>,
    /// Updated whenever geometry changes so a notification received after a
    /// shared model mutation can still restore the pre-mutation stable key.
    last_anchor: Option<SparseAnchor<TreeRowKey>>,
    interaction: crate::interaction::InteractionEnvironment,
    /// Spacing tokens of the theme seen at the last layout.
    spacing: crate::tokens::Spacing,
    /// Leading inset of every row: the list's `s2` inset, resolved from the
    /// theme at layout time, so the depth-0 caret never sits on the pane edge.
    row_inset: f64,
    /// Gap between a row's label and its trailing content.
    row_gap: f64,
    on_rename: Option<Box<dyn FnMut(NodeId, String)>>,
    rename_on_double_click: bool,
    rename: Option<editing::RenameField>,
    tab_indents: bool,
    on_delete: Option<Box<dyn FnMut(NodeId)>>,
    context_menu_items: Option<TreeContextMenu<M>>,
    on_context_menu_item: Option<Box<dyn FnMut(Option<NodeId>, usize)>>,
    menu: ContextMenu,
    menu_target: Option<NodeId>,
    row_actions: Vec<TreeRowAction>,
    on_row_action: Option<Box<dyn FnMut(NodeId, usize)>>,
    heading_action: Option<TreeHeadingAction>,
    on_heading_action: Option<Box<dyn FnMut()>>,
    row_hint: Option<String>,
    tooltips_suppressed: bool,
    indent_guides: bool,
    /// A row to reveal once a filling tree first has a viewport.
    pending_reveal: Option<NodeId>,
    /// Set by changes the view signature cannot see (model content, layout,
    /// input the tree acted on); cleared once the tree is drawn.
    paint_dirty: bool,
    layout_theme: Option<Theme>,
    /// [`Self::paint_signature`] as of the last completed draw.
    drawn_signature: Option<u64>,
    /// The last draw's rows, so a change damages the rows it touched.
    paint_damage: crate::widget::paint_damage::PaintDamage,
    /// Where the rename field painted in the last draw.
    rename_paint: Option<Rect>,
    /// [`Self::rename_paint`] as the last drawn frame painted it.
    drawn_rename_paint: Option<Rect>,
    /// Set by `NavList`: rows paint as navigation rows.
    nav: Option<NavPresentation<M>>,
    /// The tree has been laid out at least once. Until then it has no rows
    /// on screen to move from, so model changes do not animate.
    laid_out: bool,
    /// A drag staged for a capture, applied once rows have geometry: the
    /// dragged row and the row it hovers before.
    staged_drag: Option<(NodeId, NodeId)>,
    /// The node rows of the accessibility tree the last walk handed over,
    /// kept with what they were built from ([`Self::access_rows`]).
    access_rows: std::cell::RefCell<access_rows::KeptAccessRows>,
    /// The node rows [`Self::access_rows`] built rather than kept.
    #[cfg(any(test, feature = "test-support"))]
    access_row_builds: std::cell::Cell<usize>,
}

/// Matches `theme.tokens.spacing.s4` (16.0); also the per-depth indent unit.
const DISCLOSURE_WIDTH: f64 = 16.0;
/// Indent per depth before a theme is seen: the `s4` step, unscaled.
const DEFAULT_INDENT: f64 = crate::tokens::Spacing::BASE.s4 as f64;
/// A dense row before a theme is seen: one `Body` line and its `s1` pads,
/// rounded to whole pixels.
const ROW_HEIGHT: f64 = 28.0;
/// Label-to-trailing-content gap before a theme is seen: the `s2` step.
const DEFAULT_ROW_GAP: f64 = crate::tokens::Spacing::BASE.s2 as f64;
const MAX_TRANSITION_ROWS: usize = 512;
/// The role every row label is set in; the rename field matches it.
const LABEL_ROLE: TextRole = TextRole::Body;

/// A branch row's disclosure: a chevron from the icon set at the small
/// glyph size, at the start of the disclosure `column` on the row's centre
/// line, so the rest of the column is air before the label.
fn paint_disclosure(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    column: Rect,
    expanded: bool,
    opacity: f32,
) {
    let edge = f64::from(theme.tokens.metrics.icon_sm);
    let centre = Point::new(
        column.x0 + edge.min(column.width()) / 2.0,
        column.center().y,
    );
    crate::icons::draw_icon(
        scene,
        transform,
        Rect::from_center_size(centre, (edge, edge)),
        if expanded {
            crate::icons::Icon::ChevronDown
        } else {
            crate::icons::Icon::ChevronRight
        },
        theme.tokens.colors.text_dim.with_alpha_factor(opacity),
        crate::icons::LUCIDE_DEFAULT_STROKE_PX,
    );
}

type TreeRowDelegate<M> = Box<dyn FnMut(&M, NodeId, usize) -> Element>;
type TreeRowDecorator<M> = Box<dyn FnMut(&M, TreeRowInfo, &Theme) -> TreeRowDecoration>;
type TreeSlotDelegate<M> =
    Box<dyn for<'a> FnMut(&M, TreeSlot, usize, TreeSlotState<'a>) -> Element>;
type TreeReorderValidator<M> = Box<dyn FnMut(&M, NodeId, &DropTarget<NodeId>) -> bool>;
type TreeReorderCommit<M> = Box<dyn FnMut(&mut M, ReorderDrop<NodeId>) -> bool>;
type TreeContextMenu<M> = Box<dyn FnMut(&M, Option<NodeId>) -> Vec<MenuItem>>;

struct MountedTreeRow {
    key: TreeRowKey,
    depth: usize,
    index: usize,
    widget: Element,
    node: Option<taffy::NodeId>,
}

#[derive(Clone, Copy)]
enum TransitionTextTone {
    Primary,
    Dim,
    Danger,
}

#[derive(Clone)]
struct TreeTransitionPayload {
    label: String,
    depth: usize,
    is_branch: bool,
    expanded: bool,
    selected: bool,
    tone: TransitionTextTone,
}

impl<T> TreeView<T, TreeNode<T>> {
    pub fn new(root: TreeNode<T>) -> Self {
        Self::from_source(root)
    }

    /// Batch edits to the convenient owned model and reconcile once.
    pub fn update_model<R>(&mut self, update: impl FnOnce(&mut TreeNode<T>) -> R) -> R {
        let anchor = self.capture_view_anchor();
        let result = update(&mut self.model);
        self.invalidate_all_delegates();
        self.reconcile_model_state(anchor.as_ref());
        result
    }

    /// Enable safe before/inside/after dragging for the owned [`TreeNode`]
    /// model. Subtrees can be reordered among siblings or reparented without
    /// app-authored detach/insert bookkeeping.
    #[must_use]
    pub fn reorderable(self) -> Self
    where
        T: 'static,
    {
        self.with_reorder(TreeNode::can_reorder, |model, dropped| {
            model.reorder(&dropped)
        })
    }
}

impl<M: TreeModel> TreeView<(), M> {
    /// Build a view over an app-owned, database-backed, or asynchronous model.
    /// Call [`Self::notify_model_changes`] after the source applies a batch.
    pub fn from_model(model: M) -> Self {
        Self::from_source(model)
    }
}

impl<T, M: TreeModel> TreeView<T, M> {
    fn from_source(model: M) -> Self {
        let expanded = HashSet::new();
        let index = SparseTreeIndex::build(&model, &expanded, false);
        let mut viewport = SparseVirtualViewport::new(ROW_HEIGHT);
        viewport.set_overscan(viewport.estimated_height() * 2.0);
        viewport.reconcile(index.len(), |_| None, None);
        let mut view = Self {
            test_id: None,
            model,
            data: PhantomData,
            expanded,
            selection: crate::selection::KeySelection::new(),
            multi_select: false,
            focused: None,
            typing: crate::type_ahead::TypeAhead::default(),
            focused_affordance: None,
            affordance_focus_valid: false,
            on_select: None,
            on_expand: None,
            on_load_request: None,
            on_activate: None,
            click_activation: TreeClickActivation::default(),
            last_click: None,
            selection_follows_focus: false,
            row_decoration: None,
            current: None,
            heading: None,
            heading_h: 0.0,
            ground: None,
            fill: false,
            hovered: None,
            row_descriptions: HashMap::new(),
            reorder: ReorderController::new(),
            reorder_validator: None,
            reorder_commit: None,
            drag_scroll: DragScroller::new(),
            momentum: ScrollMomentum::new(),
            last_tick: None,
            transition_snapshot: Vec::new(),
            transitions: KeyedTransitionSet::new(),
            transition_motion: crate::tokens::EffectiveMotion::default(),
            reduce_motion: false,
            disclosure_w: DISCLOSURE_WIDTH,
            authored_row_height: None,
            row_type: None,
            indent_px: DEFAULT_INDENT,
            requested_slots: HashSet::new(),
            row_delegate: None,
            slot_delegate: None,
            mounted_rows: Vec::new(),
            delegate_structure_dirty: false,
            delegate_layout_dirty: false,
            accessible_name: None,
            rect: Rect::ZERO,
            is_focused: false,
            last_cursor: None,
            bar_fade: std::cell::Cell::default(),
            bar_offset: std::cell::Cell::new(None),
            bar_held: std::cell::Cell::new(false),
            index,
            viewport,
            last_anchor: None,
            interaction: crate::interaction::InteractionEnvironment::default(),
            spacing: crate::tokens::Spacing::BASE,
            row_inset: DEFAULT_ROW_GAP,
            row_gap: DEFAULT_ROW_GAP,
            on_rename: None,
            rename_on_double_click: false,
            rename: None,
            tab_indents: false,
            on_delete: None,
            context_menu_items: None,
            on_context_menu_item: None,
            menu: ContextMenu::new(),
            menu_target: None,
            row_actions: Vec::new(),
            on_row_action: None,
            heading_action: None,
            on_heading_action: None,
            row_hint: None,
            tooltips_suppressed: false,
            indent_guides: false,
            pending_reveal: None,
            paint_dirty: true,
            layout_theme: None,
            drawn_signature: None,
            paint_damage: crate::widget::paint_damage::PaintDamage::default(),
            rename_paint: None,
            drawn_rename_paint: None,
            nav: None,
            laid_out: false,
            staged_drag: None,
            access_rows: std::cell::RefCell::default(),
            #[cfg(any(test, feature = "test-support"))]
            access_row_builds: std::cell::Cell::new(0),
        };
        view.refresh_anchor();
        view.refresh_transition_snapshot();
        view
    }

    /// Give this tree a stable app-test/inspection id.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn on_select<F: FnMut(NodeId) + 'static>(mut self, f: F) -> Self {
        self.on_select = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn on_expand<F: FnMut(NodeId, bool) + 'static>(mut self, f: F) -> Self {
        self.on_expand = Some(Box::new(f));
        self
    }

    /// Receive demand-driven page requests. A loading slot is requested once
    /// when it enters the paint window; an error slot requests again only when
    /// the user activates it.
    #[must_use]
    pub fn on_load_request<F: FnMut(TreeLoadRequest) + 'static>(mut self, f: F) -> Self {
        self.on_load_request = Some(Box::new(f));
        self
    }

    /// Receive row activations — the "open this" gesture, distinct from
    /// selection. `Enter`, the accessibility `Activate` verb and the pointer
    /// gesture chosen by [`Self::click_activation`] activate a node; arrow
    /// keys, `Space` and focus changes never do, so keyboard browsing cannot
    /// open every row it passes. Activation also selects the row.
    #[must_use]
    pub fn on_activate<F: FnMut(NodeId) + 'static>(mut self, f: F) -> Self {
        self.on_activate = Some(Box::new(f));
        self
    }

    /// Choose which pointer gesture activates a row (double click by
    /// default).
    #[must_use]
    pub fn click_activation(mut self, activation: TreeClickActivation) -> Self {
        self.click_activation = activation;
        self
    }

    /// Make keyboard focus movement (arrows, Home/End, Page Up/Down) select
    /// the row it lands on, firing [`Self::on_select`] but never
    /// [`Self::on_activate`] — the platform list convention for navigator
    /// sidebars.
    #[must_use]
    pub fn selection_follows_focus(mut self, enabled: bool) -> Self {
        self.selection_follows_focus = enabled;
        self
    }

    /// Decorate built-in rows with an icon, a tone, trailing text and an
    /// accessible description, recomputed each frame from the model and the
    /// active theme. Rows mounted through [`Self::with_row_delegate`] are not
    /// decorated.
    #[must_use]
    pub fn with_row_decoration<F>(mut self, decorate: F) -> Self
    where
        F: FnMut(&M, TreeRowInfo, &Theme) -> TreeRowDecoration + 'static,
    {
        self.row_decoration = Some(Box::new(decorate));
        self
    }

    /// Paint a section heading above the rows (a sidebar's "Explorer" or
    /// "Schema" label). The heading also names the tree for assistive
    /// technology unless [`Self::with_accessible_name`] overrides it.
    #[must_use]
    pub fn with_heading(mut self, heading: impl Into<String>) -> Self {
        self.heading = Some(heading.into());
        self
    }

    /// Replace the heading (see [`Self::with_heading`]).
    pub fn set_heading(&mut self, heading: Option<String>) {
        if self.heading != heading {
            self.heading = heading;
            self.paint_dirty = true;
        }
    }

    /// Paint the tree on a surface other than `surfaces.base` (a raised
    /// sidebar). The function is resolved against the theme every frame, so
    /// the ground tracks theme changes; hover and selection lift from it.
    #[must_use]
    pub fn with_ground(mut self, ground: fn(&Theme) -> Color) -> Self {
        self.ground = Some(ground);
        self
    }

    /// Hide the root: its children form the top level at depth 0 and it is
    /// always expanded. Suits a project or database whose name is already
    /// the heading.
    #[must_use]
    pub fn hide_root(mut self) -> Self {
        if !self.index.root_hidden {
            let anchor = self.capture_view_anchor();
            self.index.root_hidden = true;
            self.reconcile_visible(anchor.as_ref());
        }
        self
    }

    /// Take the height the parent allots (or the rect given to
    /// [`Self::place_at`]) and scroll internally, instead of sizing to the
    /// content for an outer `Scrollable`.
    #[must_use]
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self
    }

    /// Mark the host's current item — the open file, the table on screen.
    /// The current row's label is set in the heavier weight; it is
    /// independent of selection, which follows the user's browsing.
    pub fn set_current(&mut self, id: Option<NodeId>) {
        self.current = id.filter(|id| self.model.contains(*id));
    }

    #[must_use]
    pub fn current(&self) -> Option<NodeId> {
        self.current
    }

    /// Position the tree at an externally computed `rect` without a taffy
    /// pass — for a sidebar that slides over the content, whose geometry the
    /// host derives each frame. Paint, hit-testing and semantics all follow
    /// the new rect; a [`Self::fill`] tree also resizes its viewport to it.
    /// Rows mounted through [`Self::with_row_delegate`] keep the geometry of
    /// their last layout pass, so external placement suits built-in and
    /// decorated rows.
    pub fn place_at(&mut self, rect: Rect) {
        self.rect = rect;
        if self.fill {
            self.fit_viewport_to_rect();
        }
        self.laid_out = true;
        self.refresh_anchor();
        self.apply_staged_drag();
    }

    /// Replace the expanded set without firing [`Self::on_expand`] — for a
    /// host restoring expansion it persists itself. Unknown ids, and leaves
    /// that cannot [hold children](TreeModel::accepts_children), are ignored.
    pub fn restore_expanded<I>(&mut self, ids: I)
    where
        I: IntoIterator<Item = NodeId>,
    {
        let next = ids
            .into_iter()
            .filter(|id| self.model.is_branch(*id) || self.model.accepts_children(*id))
            .collect::<HashSet<_>>();
        if next != self.expanded {
            let anchor = self
                .last_anchor
                .clone()
                .or_else(|| self.capture_view_anchor());
            self.expanded = next;
            self.reconcile_visible(anchor.as_ref());
        }
    }

    /// The expanded nodes, for a host persisting expansion itself.
    pub fn expanded_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.expanded.iter().copied()
    }

    /// Scroll `id`'s row into view if it is in the visible order. A
    /// [`Self::fill`] tree that has no viewport yet reveals the row once its
    /// first layout sizes one.
    pub fn reveal(&mut self, id: NodeId) {
        if self.fill && self.viewport.viewport() <= 0.0 {
            self.pending_reveal = Some(id);
            return;
        }
        self.pending_reveal = None;
        if let Some(index) = self.visible_index_of(id) {
            self.viewport
                .scroll_to_index(index, ScrollAlignment::Nearest);
            self.mark_delegate_window_dirty();
            self.refresh_anchor();
        }
    }

    /// Author the row height at the default text scale (the estimate
    /// [`Self::set_viewport`] takes, without fixing a viewport height). It
    /// grows only once the label line itself would not fit.
    #[must_use]
    pub fn with_row_height(mut self, height_px: f32) -> Self {
        self.authored_row_height = Some(f64::from(height_px));
        self.refresh_row_estimate();
        self
    }

    /// The painted rect of visible row `index`, clipped to nothing when it is
    /// outside the viewport.
    fn visible_row_rect(&self, index: usize) -> Option<Rect> {
        let top = self.viewport.item_view_top(index)?;
        let height = self.viewport.item_height(index)?;
        let y0 = self.body_top() + top;
        let y1 = y0 + height;
        let body_bottom = self.body_top() + self.body_height();
        (y1 > self.body_top() && y0 < body_bottom)
            .then(|| Rect::new(self.rect.x0, y0, self.rect.x1, y1))
    }

    /// Enable validated before/inside/after node reordering. A successful
    /// commit mutates the model and returns `true`; the tree then reconciles
    /// expansion, selection, retained delegates, and its stable viewport
    /// anchor once.
    #[must_use]
    pub fn with_reorder(
        mut self,
        validate: impl FnMut(&M, NodeId, &DropTarget<NodeId>) -> bool + 'static,
        commit: impl FnMut(&mut M, ReorderDrop<NodeId>) -> bool + 'static,
    ) -> Self {
        self.reorder_validator = Some(Box::new(validate));
        self.reorder_commit = Some(Box::new(commit));
        self
    }

    #[must_use]
    pub fn reorder_target(&self) -> Option<&DropTarget<NodeId>> {
        self.reorder.target()
    }

    #[must_use]
    pub fn reorder_preview(&self) -> Option<Rect> {
        self.reorder.preview()
    }

    /// Replace built-in text rows with retained widget subtrees for resident
    /// model nodes. Disclosure, selection, focus, indentation, loading/error
    /// placeholders, and tree semantics remain owned by `TreeView`.
    #[must_use]
    pub fn with_row_delegate<F>(mut self, delegate: F) -> Self
    where
        F: FnMut(&M, NodeId, usize) -> Element + 'static,
    {
        self.row_delegate = Some(Box::new(delegate));
        self.delegate_structure_dirty = true;
        self.delegate_layout_dirty = true;
        self
    }

    /// Replace built-in loading and error text with retained widget subtrees.
    ///
    /// The delegate is mounted only for unresolved slots in the virtual paint
    /// window. `TreeView` continues to own stable row identity, busy/invalid
    /// semantics, focus, activation-to-retry, measurement, and request
    /// deduplication. This lets apps localize placeholders or add ordinary
    /// progress and retry controls without turning them into model nodes.
    #[must_use]
    pub fn with_slot_delegate<F>(mut self, delegate: F) -> Self
    where
        F: for<'a> FnMut(&M, TreeSlot, usize, TreeSlotState<'a>) -> Element + 'static,
    {
        self.slot_delegate = Some(Box::new(delegate));
        self.delegate_structure_dirty = true;
        self.delegate_layout_dirty = true;
        self
    }

    /// Rebuild delegated widgets for model nodes whose displayed data changed.
    pub fn invalidate_row_delegates(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        let ids = ids.into_iter().collect::<HashSet<_>>();
        let before = self.mounted_rows.len();
        self.mounted_rows
            .retain(|row| !matches!(row.key, TreeRowKey::Node(id) if ids.contains(&id)));
        if self.mounted_rows.len() != before {
            self.delegate_structure_dirty = true;
        }
    }

    fn invalidate_all_delegates(&mut self) {
        if !self.mounted_rows.is_empty() {
            self.mounted_rows.clear();
            self.delegate_structure_dirty = true;
        }
    }

    /// Seed expansion without firing [`Self::on_expand`]. Unknown and leaf
    /// ids are ignored, which keeps generated/app-owned state tolerant of a
    /// model that has changed since it was persisted.
    #[must_use]
    pub fn with_expanded<I>(mut self, ids: I) -> Self
    where
        I: IntoIterator<Item = NodeId>,
    {
        for id in ids {
            if self.model.is_branch(id) {
                self.expanded.insert(id);
            }
        }
        self.reconcile_visible(None);
        self
    }

    #[must_use]
    pub fn with_selected(mut self, id: Option<NodeId>) -> Self {
        if let Some(id) = id.filter(|id| self.model.contains(*id)) {
            self.selection.select_only(id);
        } else {
            self.selection.clear();
        }
        self
    }

    /// Enable platform-standard Ctrl/Command toggle, Shift range, and
    /// Ctrl/Command+A selection. Single selection remains the default.
    #[must_use]
    pub fn multi_select(mut self, enabled: bool) -> Self {
        self.multi_select = enabled;
        self
    }

    #[must_use]
    pub fn with_selection(mut self, selection: crate::selection::KeySelection<NodeId>) -> Self {
        self.selection = selection;
        self.selection.reconcile(|id| self.model.contains(*id));
        self
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    pub fn is_expanded(&self, id: NodeId) -> bool {
        self.expanded.contains(&id)
    }

    pub fn toggle_expand(&mut self, id: NodeId) {
        let expanded = !self.expanded.contains(&id);
        self.set_expanded(id, expanded);
    }

    /// Expand or collapse a branch and report whether state changed.
    pub fn set_expanded(&mut self, id: NodeId, expanded: bool) -> bool {
        if !self.model.is_branch(id) || (!expanded && !self.model.can_collapse(id)) {
            return false;
        }
        let changed = if expanded {
            self.expanded.insert(id)
        } else {
            self.expanded.remove(&id)
        };
        if changed {
            let anchor = self
                .last_anchor
                .clone()
                .or_else(|| self.capture_view_anchor());
            self.reconcile_visible(anchor.as_ref());
            if let Some(callback) = &mut self.on_expand {
                callback(id, expanded);
            }
        }
        changed
    }

    /// Does not require `id` to be visible: collapsing a parent doesn't
    /// clear the selection.
    pub fn select(&mut self, id: NodeId) {
        if !self.model.is_selectable(id) {
            return;
        }
        self.selection.select_only(id);
        if let Some(cb) = self.on_select.as_mut() {
            cb(id);
        }
    }

    pub fn selected(&self) -> Option<NodeId> {
        self.selection
            .primary()
            .copied()
            .filter(|id| self.selection.is_selected(id))
    }

    #[must_use]
    pub fn selection(&self) -> &crate::selection::KeySelection<NodeId> {
        &self.selection
    }

    pub fn selection_mut(&mut self) -> &mut crate::selection::KeySelection<NodeId> {
        &mut self.selection
    }

    pub fn select_all(&mut self) {
        self.selection.select_all();
    }

    pub fn focused(&self) -> Option<NodeId> {
        self.focused.and_then(|key| match key {
            TreeRowKey::Node(id) => Some(id),
            TreeRowKey::Slot(_) => None,
        })
    }

    #[must_use]
    pub fn focused_row(&self) -> Option<TreeRowKey> {
        self.focused
    }

    /// Move the keyboard focus ring to `id` without selecting it — for a
    /// host whose selection changed elsewhere, so arrow keys continue from
    /// the row it now shows as chosen.
    pub fn set_focused(&mut self, id: Option<NodeId>) {
        self.focused_affordance = None;
        self.focused = id
            .filter(|id| self.model.contains(*id))
            .map(TreeRowKey::Node);
    }

    /// Whether a row is being dragged (past the drag threshold).
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.reorder.is_active()
    }

    /// Whether rows are moving between geometries now.
    #[cfg(test)]
    pub(crate) fn transitions_active(&self) -> bool {
        self.transitions.is_active(crate::time::now())
    }

    fn reconcile_model_state(&mut self, anchor: Option<&SparseAnchor<TreeRowKey>>) {
        self.affordance_focus_valid = false;
        let anchor = anchor.map(|anchor| self.normalize_anchor(anchor.clone()));
        // A node that accepts children stays expanded while it has none, so
        // a row dropped or indented into it is revealed once the host's model
        // catches up.
        self.expanded.retain(|id| {
            self.model.contains(*id)
                && (self.model.is_branch(*id) || self.model.accepts_children(*id))
        });
        self.selection.reconcile(|id| self.model.contains(*id));
        if self
            .rename
            .as_ref()
            .is_some_and(|field| !self.model.contains(field.id))
        {
            self.rename = None;
        }
        if self
            .menu_target
            .is_some_and(|target| !self.model.contains(target))
        {
            self.close_context_menu();
        }
        self.focused = self.focused.filter(|key| match key {
            TreeRowKey::Node(id) => self.model.contains(*id),
            TreeRowKey::Slot(slot) => {
                self.model.contains(slot.parent) && slot.index < self.model.child_count(slot.parent)
            }
        });
        self.reconcile_visible(anchor.as_ref());
    }

    #[must_use]
    pub fn model(&self) -> &M {
        &self.model
    }

    /// Mutate an app-owned model stored directly in the view and apply its
    /// notification batch afterward. Models backed by shared interior state
    /// can mutate externally and call [`Self::notify_model_changes`] instead.
    pub fn update_source<R>(
        &mut self,
        changes: &[TreeModelChange],
        update: impl FnOnce(&mut M) -> R,
    ) -> R {
        self.invalidate_changed_slots(changes);
        let reconcile = Self::changes_require_reconcile(changes);
        let anchor = reconcile.then(|| self.capture_view_anchor()).flatten();
        let result = update(&mut self.model);
        self.affordance_focus_valid = false;
        self.paint_dirty = true;
        self.invalidate_changed_delegates(changes);
        self.resolve_changed_slots(changes);
        if reconcile {
            self.reconcile_model_state(anchor.as_ref());
        }
        result
    }

    /// Reconcile a batch already applied to the model. Label-only changes keep
    /// geometry; structural and slot-state changes rebuild once and preserve
    /// the current viewport anchor by stable row identity.
    pub fn notify_model_changes(&mut self, changes: &[TreeModelChange]) {
        if !changes.is_empty() {
            self.affordance_focus_valid = false;
        }
        self.paint_dirty = true;
        self.invalidate_changed_slots(changes);
        self.invalidate_changed_delegates(changes);
        self.resolve_changed_slots(changes);
        if Self::changes_require_reconcile(changes) {
            let anchor = self.last_anchor.clone();
            self.reconcile_model_state(anchor.as_ref());
        }
    }

    fn changes_require_reconcile(changes: &[TreeModelChange]) -> bool {
        changes
            .iter()
            .any(|change| !matches!(change, TreeModelChange::RowsChanged { .. }))
    }

    fn invalidate_changed_slots(&mut self, changes: &[TreeModelChange]) {
        for change in changes {
            if let TreeModelChange::SlotsChanged { slots } = change {
                for slot in slots {
                    self.requested_slots.remove(slot);
                }
                let before = self.mounted_rows.len();
                self.mounted_rows.retain(
                    |row| !matches!(row.key, TreeRowKey::Slot(slot) if slots.contains(&slot)),
                );
                if self.mounted_rows.len() != before {
                    self.delegate_structure_dirty = true;
                    self.delegate_layout_dirty = true;
                }
            }
        }
    }

    fn invalidate_changed_delegates(&mut self, changes: &[TreeModelChange]) {
        if changes
            .iter()
            .any(|change| matches!(change, TreeModelChange::Reset))
        {
            self.invalidate_all_delegates();
            return;
        }
        let ids = changes.iter().filter_map(|change| match change {
            TreeModelChange::RowsChanged { ids } => Some(ids.iter().copied()),
            TreeModelChange::Reset
            | TreeModelChange::RowsInserted { .. }
            | TreeModelChange::RowsRemoved { .. }
            | TreeModelChange::RowsMoved { .. }
            | TreeModelChange::SlotsChanged { .. } => None,
        });
        self.invalidate_row_delegates(ids.flatten());
    }

    fn resolve_changed_slots(&mut self, changes: &[TreeModelChange]) {
        for change in changes {
            let TreeModelChange::SlotsChanged { slots } = change else {
                continue;
            };
            for slot in slots {
                let TreeChildState::Ready(id) = self.model.child_state(slot.parent, slot.index)
                else {
                    continue;
                };
                let old = TreeRowKey::Slot(*slot);
                let new = TreeRowKey::Node(id);
                self.viewport.remap_key(&old, new);
                if self.focused == Some(old) {
                    self.focused = Some(new);
                }
                if self
                    .last_anchor
                    .as_ref()
                    .is_some_and(|anchor| anchor.key() == &old)
                {
                    self.last_anchor = self.last_anchor.take().map(|anchor| anchor.with_key(new));
                }
            }
        }
    }

    fn normalize_anchor(&self, anchor: SparseAnchor<TreeRowKey>) -> SparseAnchor<TreeRowKey> {
        let TreeRowKey::Slot(slot) = *anchor.key() else {
            return anchor;
        };
        match self.model.child_state(slot.parent, slot.index) {
            TreeChildState::Ready(id) => anchor.with_key(TreeRowKey::Node(id)),
            TreeChildState::Loading | TreeChildState::Error { .. } => anchor,
        }
    }

    /// Collect every expanded row as `(id, label, depth)`. This convenience is
    /// intended for small owned trees and tests; large/paged models should use
    /// [`Self::visible_len`], [`Self::visible_row`], and
    /// [`Self::visible_row_range`] so they never materialize the full order.
    pub fn visible_rows(&self) -> Vec<(NodeId, &str, usize)> {
        (0..self.index.len())
            .filter_map(|index| self.visible_row(index))
            .collect()
    }

    #[must_use]
    pub fn visible_len(&self) -> usize {
        self.index.len()
    }

    #[must_use]
    pub fn visible_row(&self, index: usize) -> Option<(NodeId, &str, usize)> {
        let (TreeRowKey::Node(id), depth) = self.index.row_at(&self.model, index)? else {
            return None;
        };
        self.model.label(id).map(|label| (id, label, depth))
    }

    fn visible_index_of(&self, id: NodeId) -> Option<usize> {
        self.index.index_of(&self.model, id)
    }

    fn capture_view_anchor(&self) -> Option<SparseAnchor<TreeRowKey>> {
        self.viewport
            .capture_anchor(|index| self.index.row_at(&self.model, index).map(|(key, _)| key))
    }

    fn capture_transition_snapshot(
        &self,
    ) -> Vec<KeyedRowSnapshot<TreeRowKey, TreeTransitionPayload>> {
        if self.viewport.viewport() <= 0.0 && self.index.len() > MAX_TRANSITION_ROWS {
            return Vec::new();
        }
        self.visible_row_range()
            .take(MAX_TRANSITION_ROWS)
            .filter_map(|index| {
                let (key, depth) = self.index.row_at(&self.model, index)?;
                let geometry = RowGeometry::new(
                    self.viewport.item_view_top(index)?,
                    self.viewport.item_height(index)?,
                );
                let (label, is_branch, expanded, tone) = match key {
                    TreeRowKey::Node(id) => (
                        self.model.label(id)?.to_owned(),
                        self.model.is_branch(id),
                        self.is_expanded(id),
                        TransitionTextTone::Primary,
                    ),
                    TreeRowKey::Slot(slot) => match self.model.child_state(slot.parent, slot.index) {
                        TreeChildState::Ready(_) => return None,
                        TreeChildState::Loading | TreeChildState::Error { .. }
                            if self.slot_delegate.is_some() =>
                        {
                            return None;
                        }
                        TreeChildState::Loading => (
                            "Loading…".to_owned(),
                            false,
                            false,
                            TransitionTextTone::Dim,
                        ),
                        TreeChildState::Error { message } => (
                            message
                                .unwrap_or("Could not load. Activate to retry")
                                .to_owned(),
                            false,
                            false,
                            TransitionTextTone::Danger,
                        ),
                    },
                };
                Some(KeyedRowSnapshot {
                    key,
                    geometry,
                    payload: TreeTransitionPayload {
                        label,
                        depth,
                        is_branch,
                        expanded,
                        selected: matches!(key, TreeRowKey::Node(id) if self.selection.is_selected(&id)),
                        tone,
                    },
                })
            })
            .collect()
    }

    fn refresh_transition_snapshot(&mut self) {
        self.transition_snapshot = self.capture_transition_snapshot();
    }

    fn refresh_anchor(&mut self) {
        self.last_anchor = self.capture_view_anchor();
    }

    fn reconcile_visible(&mut self, anchor: Option<&SparseAnchor<TreeRowKey>>) {
        self.paint_dirty = true;
        let old_snapshot = self.transition_snapshot.clone();
        let next = SparseTreeIndex::build(&self.model, &self.expanded, self.index.root_hidden);
        self.viewport.reconcile(
            next.len(),
            |key| next.index_of_key(&self.model, *key),
            anchor,
        );
        self.index = next;
        self.refresh_nav_heights();
        let new_snapshot = self.capture_transition_snapshot();
        if self.laid_out && !self.is_nav() {
            self.transitions.reconcile(
                &old_snapshot,
                &new_snapshot,
                crate::time::now(),
                self.transition_motion,
                crate::tokens::MotionSource::Pointer,
            );
        } else {
            // Navigation rows and section headings must share the same geometry
            // as their hit and accessibility targets immediately. Moving old
            // headings through full-height arriving rows causes live overlap.
            // Generic trees retain their structural transitions above.
            self.transitions.clear();
        }
        self.transition_snapshot = new_snapshot;
        self.mark_delegate_window_dirty();
        self.refresh_anchor();
    }

    fn mark_delegate_window_dirty(&mut self) {
        if self.row_delegate.is_some() || self.slot_delegate.is_some() {
            self.delegate_structure_dirty = true;
            self.delegate_layout_dirty = true;
        }
    }

    /// Fluent form of [`Self::set_viewport`] for app constructors.
    #[must_use]
    pub fn with_viewport(mut self, height_px: f32, estimated_row_height_px: f32) -> Self {
        self.set_viewport(height_px, estimated_row_height_px);
        self
    }

    /// Enable bounded internal virtualization. Without an explicit viewport,
    /// the tree keeps its historical content-sized layout and can be wrapped
    /// in an outer `Scrollable`.
    pub fn set_viewport(&mut self, height_px: f32, estimated_row_height_px: f32) {
        self.authored_row_height = Some(f64::from(estimated_row_height_px));
        self.refresh_row_estimate();
        self.viewport.set_viewport(f64::from(height_px));
        self.viewport
            .set_overscan(self.viewport.estimated_height() * 2.0);
        self.mark_delegate_window_dirty();
        self.refresh_anchor();
        self.transitions.clear();
    }

    pub fn set_scroll_y(&mut self, offset_px: f32) {
        self.set_scroll_offset(f64::from(offset_px));
    }

    /// Set a logical-pixel offset without the precision loss of `f32` at
    /// hundred-million-row extents.
    pub fn set_scroll_offset(&mut self, offset_px: f64) {
        let before = self.viewport.visible_range();
        self.viewport.set_offset(offset_px);
        self.finish_viewport_scroll(before);
    }

    fn finish_viewport_scroll(&mut self, before: std::ops::Range<usize>) {
        if self.viewport.visible_range() != before {
            self.mark_delegate_window_dirty();
        } else if self.row_delegate.is_some() {
            self.delegate_layout_dirty = true;
        }
        self.refresh_anchor();
        self.transitions.clear();
        self.refresh_transition_snapshot();
    }

    #[must_use]
    pub fn scroll_y(&self) -> f32 {
        f32_of_f64(self.scroll_offset())
    }

    #[must_use]
    pub fn scroll_offset(&self) -> f64 {
        self.viewport.offset()
    }

    #[must_use]
    pub fn max_scroll_y(&self) -> f32 {
        f32_of_f64(self.max_scroll_offset())
    }

    #[must_use]
    pub fn max_scroll_offset(&self) -> f64 {
        self.viewport.max_offset()
    }

    /// Cache the measured height of one row in current visible order.
    pub fn set_row_height(&mut self, visible_index: usize, height_px: f32) -> bool {
        let Some((key, _)) = self.index.row_at(&self.model, visible_index) else {
            return false;
        };
        let changed = self
            .viewport
            .measure(visible_index, key, f64::from(height_px));
        if changed {
            self.mark_delegate_window_dirty();
        }
        self.refresh_anchor();
        self.refresh_transition_snapshot();
        changed
    }

    #[must_use]
    pub fn visible_row_range(&self) -> std::ops::Range<usize> {
        self.viewport.visible_range()
    }

    /// Demand every loading slot in the current paint window exactly once.
    /// Hosts that drive a headless tree can call this explicitly; normal
    /// widget drawing calls it automatically.
    pub fn request_visible_rows(&mut self) -> usize {
        let slots = self
            .visible_row_range()
            .filter_map(|index| self.index.row_at(&self.model, index))
            .filter_map(|(key, _)| match key {
                TreeRowKey::Slot(slot)
                    if matches!(
                        self.model.child_state(slot.parent, slot.index),
                        TreeChildState::Loading
                    ) =>
                {
                    Some(slot)
                }
                TreeRowKey::Node(_) | TreeRowKey::Slot(_) => None,
            })
            .collect::<Vec<_>>();
        slots
            .into_iter()
            .filter(|slot| self.request_slot(*slot, TreeLoadReason::Visible))
            .count()
    }

    /// Top of the row area, below the heading band.
    fn body_top(&self) -> f64 {
        self.rect.y0 + self.heading_h
    }

    /// Size a [`Self::fill`] tree's viewport to its rect, less the heading.
    fn fit_viewport_to_rect(&mut self) {
        let height = (self.rect.height() - self.heading_h).max(0.0);
        if (self.viewport.viewport() - height).abs() > 0.25 {
            let before = self.viewport.visible_range();
            self.viewport.set_viewport(height);
            self.viewport
                .set_overscan(self.viewport.estimated_height() * 2.0);
            if self.viewport.visible_range() != before {
                self.mark_delegate_window_dirty();
            }
            self.refresh_transition_snapshot();
        }
        if height > 0.0
            && let Some(id) = self.pending_reveal.take()
        {
            self.reveal(id);
        }
    }

    fn body_height(&self) -> f64 {
        if self.viewport.viewport() > 0.0 {
            self.viewport.viewport()
        } else {
            self.viewport
                .content_extent()
                .max(self.viewport.estimated_height())
        }
    }

    /// Size the estimated row height and the disclosure column from the
    /// theme's label line. At the default text scale they resolve to the
    /// pinned `ROW_HEIGHT` / `DISCLOSURE_WIDTH`; at 2× they grow with the type
    /// so a label keeps its 1× padding and the glyph column still clears it.
    /// An estimate the host authored through [`Self::set_viewport`] is kept as
    /// written and raised only once the line plus the smallest step would not
    /// fit; heights set per row (`set_row_height`, delegates) are never
    /// touched.
    fn apply_type_metrics(&mut self, theme: &Theme) {
        let tokens = &theme.tokens;
        let line = type_line_height(&tokens.typography, LABEL_ROLE);
        self.disclosure_w = text_band(DISCLOSURE_WIDTH, line, 0.0);
        self.indent_px = f64::from(tokens.spacing.s4);
        let pad = f64::from(tokens.spacing.s1);
        self.row_type = Some(RowType {
            line,
            default_pad: pad,
            authored_pad: pad,
        });
        self.spacing = tokens.spacing;
        self.row_gap = f64::from(tokens.spacing.s2);
        self.row_inset = f64::from(tokens.spacing.s2);
        let heading_band =
            type_line_height(&tokens.typography, LABEL_ROLE) + 2.0 * f64::from(tokens.spacing.s2);
        self.heading_h = match (&self.heading, &self.heading_action) {
            // An action needs a band a pressable control fits in.
            (_, Some(_)) => heading_band.max(line + 4.0 * pad),
            (Some(_), None) => heading_band,
            (None, None) => 0.0,
        };
        self.refresh_row_estimate();
        self.apply_nav_metrics(theme);
    }

    /// The one place the viewport's estimated row height is set, from the
    /// authored estimate and the label type seen so far (see
    /// [`row_estimate`]). Called when either input changes.
    fn refresh_row_estimate(&mut self) {
        let estimate = row_estimate(self.authored_row_height, ROW_HEIGHT, self.row_type);
        let before = self.viewport.estimated_height();
        self.viewport.set_estimated_height(estimate);
        if self.viewport.estimated_height().to_bits() != before.to_bits() {
            self.viewport
                .set_overscan(self.viewport.estimated_height() * 2.0);
            self.mark_delegate_window_dirty();
            self.refresh_anchor();
        }
    }

    fn reorder_destination_parent(&self, target: &DropTarget<NodeId>) -> Option<NodeId> {
        match target.placement {
            DropPlacement::Inside => Some(target.key),
            DropPlacement::Before | DropPlacement::After => self.model.parent_id(target.key),
        }
    }

    fn reorder_would_cycle(&self, source: NodeId, target: &DropTarget<NodeId>) -> bool {
        let Some(mut parent) = self.reorder_destination_parent(target) else {
            return true;
        };
        let mut visited = HashSet::new();
        loop {
            if parent == source {
                return true;
            }
            if !visited.insert(parent) {
                return true;
            }
            let Some(next) = self.model.parent_id(parent) else {
                return false;
            };
            parent = next;
        }
    }

    fn reorder_candidate(&self, cursor: Point) -> Option<DropCandidate<NodeId>> {
        if !self.rect.contains(cursor) || self.index.len() == 0 {
            return None;
        }
        let local_y = cursor.y - self.body_top();
        let index = self.viewport.hit_test(local_y)?;
        let (TreeRowKey::Node(id), depth) = self.index.row_at(&self.model, index)? else {
            return None;
        };
        let top = self.viewport.item_view_top(index)?;
        let height = self.viewport.item_height(index)?.max(1.0);
        let fraction = ((local_y - top) / height).clamp(0.0, 1.0);
        let placement = if fraction < 0.25 {
            DropPlacement::Before
        } else if fraction > 0.75 || !self.model.accepts_children(id) {
            DropPlacement::After
        } else {
            DropPlacement::Inside
        };
        // The seam below an expanded branch sits above its first child: a
        // drop there lands as that child's predecessor, where the line is
        // drawn, not as the branch's next sibling far below.
        let (id, placement, depth, seam) = match (placement, self.model.child_id(id, 0)) {
            (DropPlacement::After, Some(first)) if self.is_expanded(id) => (
                first,
                DropPlacement::Before,
                depth.saturating_add(1),
                top + height,
            ),
            (DropPlacement::Before, _) => (id, placement, depth, top),
            _ => (id, placement, depth, top + height),
        };
        let indent = self.row_inset + f64_of_usize(depth) * self.indent_px;
        let preview = match placement {
            DropPlacement::Before | DropPlacement::After => Rect::new(
                self.rect.x0 + indent,
                self.body_top() + seam - 1.0,
                self.rect.x1,
                self.body_top() + seam + 1.0,
            ),
            DropPlacement::Inside => Rect::new(
                self.rect.x0 + indent,
                self.body_top() + top,
                self.rect.x1,
                self.body_top() + top + height,
            ),
        };
        Some(DropCandidate {
            target: DropTarget::new(id, placement),
            preview,
            can_hover_expand: placement == DropPlacement::Inside
                && self.model.is_branch(id)
                && !self.is_expanded(id),
        })
    }

    fn reorder_source_at_cursor(&self) -> Option<NodeId> {
        let cursor = self.last_cursor?;
        if !self.rect.contains(cursor) {
            return None;
        }
        let index = self.viewport.hit_test(cursor.y - self.body_top())?;
        let (TreeRowKey::Node(id), _) = self.index.row_at(&self.model, index)? else {
            return None;
        };
        let on_disclosure = self.model.is_branch(id)
            && self
                .disclosure_rect(id)
                .is_some_and(|rect| rect.contains(cursor));
        let on_action = self.row_actions_visible(TreeRowKey::Node(id))
            && self.row_rect(id).is_some_and(|row| {
                self.row_action_rects(row)
                    .iter()
                    .any(|r| r.contains(cursor))
            });
        (!on_disclosure && !on_action).then_some(id)
    }

    /// Whether the pointer is over a branch's disclosure column.
    fn disclosure_at_cursor(&self) -> bool {
        let Some(cursor) = self.last_cursor else {
            return false;
        };
        match self.row_key_at(cursor) {
            Some(TreeRowKey::Node(id)) if self.model.is_branch(id) => self
                .disclosure_rect(id)
                .is_some_and(|rect| rect.contains(cursor)),
            _ => false,
        }
    }

    fn update_reorder_pointer(&mut self, cursor: Point, now: Instant) {
        let candidate = self.reorder_candidate(cursor);
        let safe_candidate = candidate.filter(|candidate| {
            self.reorder
                .source()
                .is_some_and(|source| !self.reorder_would_cycle(*source, &candidate.target))
        });
        let model = &self.model;
        let validator = &mut self.reorder_validator;
        self.reorder
            .update(cursor, safe_candidate, now, |source, target| {
                validator
                    .as_mut()
                    .is_some_and(|validate| validate(model, *source, target))
            });
    }

    fn commit_reorder(&mut self, dropped: ReorderDrop<NodeId>) -> bool {
        let anchor = self.capture_view_anchor();
        let target = dropped.target.clone();
        let changed = self
            .reorder_commit
            .as_mut()
            .is_some_and(|commit| commit(&mut self.model, dropped));
        // Reveal the moved row inside its new parent.
        let opened = changed
            && target.placement == DropPlacement::Inside
            && self.model.accepts_children(target.key)
            && self.expanded.insert(target.key);
        if changed {
            self.reconcile_model_state(anchor.as_ref());
        }
        if opened && let Some(callback) = &mut self.on_expand {
            callback(target.key, true);
        }
        changed
    }

    /// Stage a drag of `source` hovering just before `before`, for a
    /// capture: the row lifts and follows a pointer resting on the seam
    /// above `before`, the drop line shows, and nothing is committed unless
    /// a real release follows (Escape cancels it). Staged before the first
    /// layout, it applies once the rows have geometry.
    pub fn stage_reorder(&mut self, source: NodeId, before: NodeId) {
        self.staged_drag = Some((source, before));
        self.apply_staged_drag();
    }

    fn apply_staged_drag(&mut self) {
        let Some((source, before)) = self.staged_drag else {
            return;
        };
        if !self.laid_out {
            return;
        }
        let (Some(from), Some(to)) = (self.row_rect(source), self.row_rect(before)) else {
            return;
        };
        self.staged_drag = None;
        self.reorder.begin(source, from.center());
        // Inside the top quarter of `before`: the seam above it.
        let at = Point::new(to.center().x, to.y0 + to.height() * 0.1);
        self.last_cursor = Some(at);
        self.hovered = None;
        self.update_reorder_pointer(at, crate::time::now());
        self.paint_dirty = true;
    }

    fn reveal_focus(&mut self) {
        if let Some(index) = self
            .focused
            .and_then(|key| self.index.index_of_key(&self.model, key))
        {
            self.viewport
                .scroll_to_index(index, ScrollAlignment::Nearest);
            self.mark_delegate_window_dirty();
            self.refresh_anchor();
        }
    }

    fn apply_selection_gesture(&mut self, id: NodeId, toggle: bool, extend: bool) {
        if !self.model.is_selectable(id) {
            return;
        }
        if !self.multi_select {
            self.select(id);
            return;
        }
        if extend {
            let Some(anchor) = self.selection.anchor().copied() else {
                self.select(id);
                return;
            };
            let (Some(anchor_index), Some(target_index)) =
                (self.visible_index_of(anchor), self.visible_index_of(id))
            else {
                self.select(id);
                return;
            };
            let lo = anchor_index.min(target_index);
            let hi = anchor_index.max(target_index);
            let keys = (lo..=hi).filter_map(|index| {
                self.index
                    .row_at(&self.model, index)
                    .and_then(|(key, _)| match key {
                        TreeRowKey::Node(id) => Some(id),
                        TreeRowKey::Slot(_) => None,
                    })
            });
            self.selection.select_range(id, keys, toggle);
        } else if toggle {
            self.selection.toggle(id);
        } else {
            self.selection.select_only(id);
        }
        if let Some(callback) = &mut self.on_select {
            callback(id);
        }
    }

    fn on_left_press(
        &mut self,
        node_id: taffy::NodeId,
        focus_target: &mut Option<taffy::NodeId>,
        toggle: bool,
        extend: bool,
    ) -> bool {
        let Some(pt) = self.last_cursor else {
            return false;
        };
        if pt.x < self.rect.x0 || pt.x > self.rect.x1 {
            return false;
        }
        let Some(index) = self.viewport.hit_test(pt.y - self.body_top()) else {
            return false;
        };
        let Some((key, depth)) = self.index.row_at(&self.model, index) else {
            return false;
        };
        let glyph_x0 = self.rect.x0 + self.row_inset + f64_of_usize(depth) * self.indent_px;
        match key {
            TreeRowKey::Node(id) => {
                let is_branch = self.model.is_branch(id);
                let on_disclosure = match self
                    .row_rect(id)
                    .and_then(|row| self.nav_disclosure_rect(id, row))
                {
                    Some(nav) => nav.is_some_and(|rect| rect.contains(pt)),
                    None => pt.x >= glyph_x0 && pt.x < glyph_x0 + self.disclosure_w,
                };
                if is_branch && on_disclosure {
                    self.toggle_expand(id);
                } else if self.toggle_nav_check(id) {
                    // The whole row is the checkbox's target.
                } else if self.rename_on_double_click
                    && !toggle
                    && !extend
                    && self.is_double_click(id)
                    && self.begin_rename(id)
                {
                    // The second click renames the row the first one chose.
                    self.last_click = None;
                } else {
                    self.apply_selection_gesture(id, toggle, extend);
                    if !toggle && !extend {
                        self.register_click(id);
                    }
                }
            }
            TreeRowKey::Slot(slot) => {
                let reason = match self.model.child_state(slot.parent, slot.index) {
                    TreeChildState::Loading => Some(TreeLoadReason::Visible),
                    TreeChildState::Error { .. } => Some(TreeLoadReason::Retry),
                    TreeChildState::Ready(_) => None,
                };
                if let Some(reason) = reason {
                    self.request_slot(slot, reason);
                }
            }
        }
        self.focused = Some(key);
        *focus_target = Some(node_id);
        true
    }

    /// Count a plain click on a node toward activation under the chosen
    /// [`TreeClickActivation`].
    fn register_click(&mut self, id: NodeId) {
        let double = self.is_double_click(id);
        self.last_click = if double {
            None
        } else {
            Some((id, crate::time::now()))
        };
        match self.click_activation {
            TreeClickActivation::SingleClick => self.fire_activate(id),
            TreeClickActivation::DoubleClick if double => self.fire_activate(id),
            TreeClickActivation::DoubleClick => {}
        }
    }

    /// Whether a click on `id` now completes a double click.
    fn is_double_click(&self, id: NodeId) -> bool {
        matches!(
            self.last_click,
            Some((previous, at))
                if previous == id
                    && crate::time::now().saturating_duration_since(at)
                        < crate::gesture::multi_click_interval()
        )
    }

    fn fire_activate(&mut self, id: NodeId) {
        if !self.model.is_selectable(id) {
            return;
        }
        if let Some(callback) = &mut self.on_activate {
            callback(id);
        }
    }

    /// Select the focused node after a keyboard move when selection follows
    /// focus.
    fn follow_focus(&mut self) {
        if !self.selection_follows_focus {
            return;
        }
        if let Some(id) = self.focused()
            && self.selected() != Some(id)
            && self.model.is_selectable(id)
        {
            self.select(id);
        }
    }

    /// The row under a logical-pixel point, if it lies in the row area.
    fn row_key_at(&self, point: Point) -> Option<TreeRowKey> {
        if !self.rect.contains(point) || point.y < self.body_top() {
            return None;
        }
        let index = self.viewport.hit_test(point.y - self.body_top())?;
        self.index.row_at(&self.model, index).map(|(key, _)| key)
    }

    fn on_wheel(&mut self, delta: &MouseScrollDelta, phase: TouchPhase) -> bool {
        if self.viewport.viewport() <= 0.0 {
            return false;
        }
        self.last_tick = Some(crate::time::now());
        match self.momentum.wheel(delta, phase, self.reduce_motion) {
            WheelStep::Scroll(dy) => {
                let before = self.viewport.offset();
                self.set_scroll_offset(before + dy);
                let moved = self.viewport.offset().to_bits() != before.to_bits();
                self.momentum.wheel_applied(dy, moved);
                moved
            }
            WheelStep::Fling => true,
            WheelStep::Ignored => false,
        }
    }

    fn focus_at(&mut self, index: usize) -> bool {
        self.focused_affordance = None;
        let Some((key, _)) = self.index.row_at(&self.model, index) else {
            return false;
        };
        self.focused = Some(key);
        self.reveal_focus();
        true
    }

    fn page_focus(&mut self, forward: bool, toggle: bool, extend: bool) -> bool {
        let current = self
            .focused
            .and_then(|key| self.index.index_of_key(&self.model, key))
            .unwrap_or_else(|| {
                if forward {
                    0
                } else {
                    self.index.len().saturating_sub(1)
                }
            });
        let before = self.viewport.visible_range();
        let Some((index, scrolled)) = self.viewport.page_from(current, forward) else {
            return false;
        };
        let Some((key, _)) = self.index.row_at(&self.model, index) else {
            return false;
        };
        self.focused = Some(key);
        if extend && let TreeRowKey::Node(id) = key {
            self.apply_selection_gesture(id, toggle, true);
        }
        self.finish_viewport_scroll(before);
        scrolled || index != current
    }

    fn on_key_with_modifiers(&mut self, logical_key: &Key, toggle: bool, extend: bool) -> bool {
        let navigation = matches!(
            logical_key,
            Key::Named(
                NamedKey::ArrowUp
                    | NamedKey::ArrowDown
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::PageUp
                    | NamedKey::PageDown
                    | NamedKey::Home
                    | NamedKey::End
            )
        );
        if navigation {
            self.focused_affordance = None;
        }
        let handled = self.on_key_inner(logical_key, toggle, extend);
        if handled && navigation && !toggle && !extend {
            self.follow_focus();
        }
        handled
    }

    fn on_key_inner(&mut self, logical_key: &Key, toggle: bool, extend: bool) -> bool {
        match logical_key {
            Key::Named(NamedKey::ArrowRight) => self.key_right(),
            Key::Named(NamedKey::ArrowLeft) => self.key_left(),
            Key::Named(NamedKey::ArrowDown) => {
                let index = self
                    .focused
                    .and_then(|key| self.index.index_of_key(&self.model, key))
                    .map_or(0, |index| {
                        index
                            .saturating_add(1)
                            .min(self.index.len().saturating_sub(1))
                    });
                let index = self.skip_nav_captions(index, true);
                let moved = self.focus_at(index);
                if moved
                    && extend
                    && let Some(id) = self.focused()
                {
                    self.apply_selection_gesture(id, toggle, true);
                }
                moved
            }
            Key::Named(NamedKey::ArrowUp) => {
                let index = self
                    .focused
                    .and_then(|key| self.index.index_of_key(&self.model, key))
                    .map_or(0, |index| index.saturating_sub(1));
                let index = self.skip_nav_captions(index, false);
                let moved = self.focus_at(index);
                if moved
                    && extend
                    && let Some(id) = self.focused()
                {
                    self.apply_selection_gesture(id, toggle, true);
                }
                moved
            }
            Key::Named(NamedKey::PageDown) => self.page_focus(true, toggle, extend),
            Key::Named(NamedKey::PageUp) => self.page_focus(false, toggle, extend),
            Key::Named(NamedKey::Home) => self.focus_at(0),
            Key::Named(NamedKey::End) => self
                .index
                .len()
                .checked_sub(1)
                .is_some_and(|index| self.focus_at(index)),
            Key::Named(NamedKey::Enter) => self.activate_focus(true),
            Key::Character(text) if text == " " && !self.typing.is_typing(crate::time::now()) => {
                self.activate_focus(false)
            }
            Key::Character(text)
                if self.multi_select
                    && toggle
                    && (text.as_str() == "a" || text.as_str() == "A") =>
            {
                self.select_all();
                true
            }
            Key::Character(text) if !toggle && self.types_ahead(text) => self.type_ahead(text),
            _ => false,
        }
    }

    /// Whether `text` continues or starts a name being typed: printable
    /// characters, and a space only inside a name (alone it selects).
    fn types_ahead(&self, text: &str) -> bool {
        let printable = !text.is_empty() && text.chars().all(|c| !c.is_control());
        printable && (text != " " || self.typing.is_typing(crate::time::now()))
    }

    /// Typing in the focused list: move to the first row whose name begins
    /// with what was typed, as a person scanning a list expects.
    fn type_ahead(&mut self, text: &str) -> bool {
        self.typing.type_text(text, crate::time::now());
        let len = self.index.len();
        let labels: Vec<String> = (0..len)
            .map(|index| match self.index.row_at(&self.model, index) {
                Some((TreeRowKey::Node(id), _)) if self.model.is_selectable(id) => {
                    self.model.label(id).map(str::to_owned).unwrap_or_default()
                }
                _ => String::new(),
            })
            .collect();
        let current = self
            .focused
            .and_then(|key| self.index.index_of_key(&self.model, key));
        let Some(target) = self
            .typing
            .target(labels.iter().map(String::as_str), current)
        else {
            // Nothing begins so: the keystroke is still the list's.
            return true;
        };
        let moved = self.focus_at(target);
        if moved {
            self.follow_focus();
        }
        true
    }

    /// `Enter` (`activate`) selects and activates the focused node; `Space`
    /// only selects it. On a placeholder slot either one retries the load.
    fn activate_focus(&mut self, activate: bool) -> bool {
        let Some(key) = self.focused else {
            return false;
        };
        match key {
            TreeRowKey::Node(id) if self.toggle_nav_check(id) => true,
            TreeRowKey::Node(id) if !self.model.is_selectable(id) => {
                // A header row: Enter and Space open or close it.
                if !self.model.is_branch(id) {
                    return false;
                }
                self.toggle_expand(id);
                true
            }
            TreeRowKey::Node(id) => {
                self.select(id);
                if activate {
                    self.fire_activate(id);
                }
                true
            }
            TreeRowKey::Slot(slot) => {
                let reason = match self.model.child_state(slot.parent, slot.index) {
                    TreeChildState::Loading => TreeLoadReason::Visible,
                    TreeChildState::Error { .. } => TreeLoadReason::Retry,
                    TreeChildState::Ready(_) => return false,
                };
                self.request_slot(slot, reason)
            }
        }
    }

    fn key_right(&mut self) -> bool {
        let Some(TreeRowKey::Node(id)) = self.focused else {
            return false;
        };
        let is_branch = self.model.is_branch(id);
        if is_branch && !self.expanded.contains(&id) {
            return self.set_expanded(id, true);
        }
        let Some(index) = self.visible_index_of(id) else {
            return false;
        };
        let own_depth = self
            .index
            .row_at(&self.model, index)
            .map(|(_, depth)| depth);
        let child = self
            .index
            .row_at(&self.model, index.saturating_add(1))
            .filter(|(_, depth)| own_depth.is_some_and(|own| *depth > own))
            .map(|(id, _)| id);
        child.is_some_and(|child| {
            self.focused = Some(child);
            self.reveal_focus();
            true
        })
    }

    fn key_left(&mut self) -> bool {
        let Some(key) = self.focused else {
            return false;
        };
        let id = match key {
            TreeRowKey::Node(id) => id,
            TreeRowKey::Slot(slot) => {
                self.focused = Some(TreeRowKey::Node(slot.parent));
                self.reveal_focus();
                return true;
            }
        };
        let is_branch = self.model.is_branch(id);
        if is_branch && self.expanded.contains(&id) {
            return self.set_expanded(id, false);
        }
        let parent = self
            .model
            .parent_id(id)
            .filter(|parent| self.visible_index_of(*parent).is_some());
        parent.is_some_and(|parent| {
            self.focused = Some(TreeRowKey::Node(parent));
            self.reveal_focus();
            true
        })
    }

    fn request_slot(&mut self, slot: TreeSlot, reason: TreeLoadReason) -> bool {
        if reason == TreeLoadReason::Retry {
            self.requested_slots.remove(&slot);
        }
        if !self.requested_slots.insert(slot) {
            return false;
        }
        let request = TreeLoadRequest { slot, reason };
        let model_accepted = self.model.request_load(request);
        let callback_accepted = self.on_load_request.is_some();
        if let Some(callback) = &mut self.on_load_request {
            callback(request);
        }
        if !model_accepted && !callback_accepted {
            self.requested_slots.remove(&slot);
            return false;
        }
        true
    }

    fn reconcile_mounted_rows(&mut self) {
        let interaction = self.interaction;
        if self.row_delegate.is_none() && self.slot_delegate.is_none() {
            if !self.mounted_rows.is_empty() {
                self.mounted_rows.clear();
                self.delegate_structure_dirty = true;
            }
            return;
        }
        let rows = self
            .visible_row_range()
            .filter_map(|index| {
                self.index
                    .row_at(&self.model, index)
                    .map(|(key, depth)| (index, key, depth))
            })
            .collect::<Vec<_>>();
        let old_keys = self
            .mounted_rows
            .iter()
            .map(|row| row.key)
            .collect::<Vec<_>>();
        let mut old = std::mem::take(&mut self.mounted_rows)
            .into_iter()
            .map(|row| (row.key, row))
            .collect::<HashMap<_, _>>();
        let mut seen = HashSet::new();
        let mut mounted = Vec::with_capacity(rows.len());
        for (index, key, depth) in rows {
            if !seen.insert(key) {
                continue;
            }
            let mut row = if let Some(row) = old.remove(&key) {
                row
            } else {
                let widget = match key {
                    TreeRowKey::Node(id) => {
                        let Some(delegate) = &mut self.row_delegate else {
                            continue;
                        };
                        delegate(&self.model, id, depth)
                    }
                    TreeRowKey::Slot(slot) => {
                        let Some(delegate) = &mut self.slot_delegate else {
                            continue;
                        };
                        let state = match self.model.child_state(slot.parent, slot.index) {
                            TreeChildState::Loading => TreeSlotState::Loading,
                            TreeChildState::Error { message } => TreeSlotState::Error { message },
                            TreeChildState::Ready(_) => continue,
                        };
                        delegate(&self.model, slot, depth, state)
                    }
                };
                MountedTreeRow {
                    key,
                    depth,
                    index,
                    widget,
                    node: None,
                }
            };
            row.widget.set_interaction_environment(interaction);
            row.depth = depth;
            row.index = index;
            row.node = None;
            mounted.push(row);
        }
        if old_keys != mounted.iter().map(|row| row.key).collect::<Vec<_>>() {
            self.delegate_structure_dirty = true;
        }
        self.mounted_rows = mounted;
    }
}

impl<T: 'static, M: TreeModel + 'static> Widget for TreeView<T, M> {
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        self.interaction = environment;
        self.viewport
            .set_minimum_height(f64::from(environment.minimum_collection_row()));
        self.viewport
            .set_overscan(self.viewport.estimated_height() * 2.0);
        for row in &mut self.mounted_rows {
            row.widget.set_interaction_environment(environment);
        }
        self.mark_delegate_window_dirty();
        self.refresh_anchor();
        self.paint_dirty = true;
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.reconcile_mounted_rows();
        for row in &mut self.mounted_rows {
            row.widget.sync(state, path);
        }
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let model_changes = self.model.poll_changes();
        let received_changes = !model_changes.is_empty();
        if received_changes {
            self.notify_model_changes(&model_changes);
        }
        let now = crate::time::now();
        let elapsed = self
            .last_tick
            .replace(now)
            .map_or(Duration::ZERO, |last| now.saturating_duration_since(last));
        let mut changed = received_changes;
        // A glide moves only the viewport, which the frame it asks for lays
        // out: no change of the model, so it does not report one (as
        // `Scrollable::update`).
        if self.momentum.is_active() {
            let delta = self.momentum.advance(now);
            let before = self.scroll_offset();
            self.set_scroll_offset(before + delta);
            if self.scroll_offset().to_bits() == before.to_bits() {
                self.momentum.stop();
            }
        }
        if self.reorder.is_active() {
            let motion = self.reorder.tick(self.rect, elapsed, now);
            if motion.scroll_delta != 0.0 {
                let before = self.scroll_offset();
                self.set_scroll_offset(before + motion.scroll_delta);
                changed |= self.scroll_offset().to_bits() != before.to_bits();
                if let Some(cursor) = self.last_cursor {
                    self.update_reorder_pointer(cursor, now);
                }
            }
            if let Some(id) = motion.hover_expand {
                changed |= self.set_expanded(id, true);
                if let Some(cursor) = self.last_cursor {
                    self.update_reorder_pointer(cursor, now);
                }
            }
        }
        for row in &mut self.mounted_rows {
            changed |= row.widget.update(state, path);
        }
        self.paint_dirty |= changed;
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        // Preserve invalidation for every theme/text-scale change, without
        // repainting the whole tree when a sibling merely needs layout.
        if self.layout_theme.as_ref() != Some(theme) {
            self.layout_theme = Some(theme.clone());
            self.paint_dirty = true;
        }
        self.transition_motion = theme.motion();
        self.reduce_motion = theme.reduce_motion;
        if self.reduce_motion {
            self.transitions.clear();
            self.momentum.stop();
        }
        self.apply_type_metrics(theme);
        self.reconcile_mounted_rows();
        let height = self.body_height();
        let disclosure_w = self.disclosure_w + self.row_inset;
        let indent_px = self.indent_px;
        let mut children = Vec::with_capacity(self.mounted_rows.len());
        for row in &mut self.mounted_rows {
            let child = row.widget.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(child).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            style.inset.left = length(f32_of_f64(
                f64_of_usize(row.depth) * indent_px + disclosure_w,
            ));
            style.inset.right = length(0.0_f32);
            style.inset.top = length(f32_of_f64(
                self.viewport.item_view_top(row.index).unwrap_or_default(),
            ));
            style.size.width = Dimension::auto();
            let _ = taffy.set_style(child, style);
            row.node = Some(child);
            children.push(child);
        }
        let (height, min_height, grow) = if self.fill {
            (percent(1.0_f32), length(0.0_f32), 1.0)
        } else {
            let content = Dimension::length(f32_of_f64(self.heading_h + height));
            (content, Dimension::auto(), 0.0)
        };
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: percent(1.0_f32),
                    height,
                },
                min_size: Size {
                    width: Dimension::auto(),
                    height: min_height,
                },
                flex_grow: grow,
                overflow: taffy::Point {
                    x: taffy::Overflow::Clip,
                    y: taffy::Overflow::Clip,
                },
                ..Default::default()
            },
            &children,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.delegate_structure_dirty = false;
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: taffy::NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if self.fill {
            self.fit_viewport_to_rect();
        }
        let child_origin = Point::new(self.rect.x0, self.body_top());
        let mut measurements = Vec::with_capacity(self.mounted_rows.len());
        for row in &mut self.mounted_rows {
            let Some(child) = row.node else {
                continue;
            };
            row.widget.apply_layout(taffy, child, child_origin);
            measurements.push((row.index, row.key, row.widget.get_rect().height()));
        }
        let mut measured = false;
        for (index, key, height) in measurements {
            measured |= self.viewport.measure(index, key, height.max(1.0));
        }
        if measured {
            self.mark_delegate_window_dirty();
        } else {
            self.delegate_layout_dirty = false;
            self.refresh_anchor();
        }
        self.laid_out = true;
        // A rail's tiles wrap their captions to the width it was given.
        if self.nav.as_ref().is_some_and(|nav| nav.rail) {
            self.refresh_nav_heights();
        }
        self.refresh_transition_snapshot();
        self.apply_staged_drag();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "tree row chrome, placeholder states, and text share one ordered paint pass"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let incoming = std::mem::take(scene);
        self.request_visible_rows();
        let tokens = &theme.tokens;
        let text_style = tokens.typography.style(LABEL_ROLE);
        self.transitions.settle(crate::time::now());
        // Every label and disclosure glyph is one line of this style, so one
        // measured line box centres every row identically.
        let (_, line_h) = text_renderer.measure_style("Tree", None, &text_style);
        // The indent and disclosure column hit-testing uses, resolved from
        // this theme at layout.
        let indent_px = self.indent_px;
        // Drop and insertion marks are the person's own colour.
        let insertion = tokens.colors.intent.accent_ink;
        let now = crate::time::now();
        // The tree stands on the ground its container handed it (the
        // region's theme re-points `surfaces.content`); only an explicit
        // `with_ground` paints one of its own.
        let ground = self
            .ground
            .map_or(tokens.colors.surfaces.content, |ground| ground(theme));
        let gap = f64::from(tokens.spacing.s2);
        let hairline = f64::from(tokens.metrics.chrome_rule_px);

        if self.ground.is_some() {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                ground,
                None,
                &self.rect,
            );
        }
        if let Some(heading) = &self.heading {
            let heading_style = tokens.typography.style(TextRole::Caption);
            let (_, heading_line) = text_renderer.measure_style(heading, None, &heading_style);
            let x = self.rect.x0 + f64::from(tokens.spacing.s3);
            let end = self
                .heading_action_rect()
                .map_or(self.rect.x1, |action| action.x0);
            let max_w = (end - x - f64::from(tokens.spacing.s3)).max(0.0);
            let heading = text_renderer.elide_to_width_style(
                heading,
                f32_of_f64(max_w),
                &heading_style
                    .clone()
                    .with_weight(crate::text_render::TextWeight::Semibold),
            );
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "kurbo Affine composition is floating-point matrix math; it cannot overflow or panic"
            )]
            let heading_xf = transform
                * Affine::translate((
                    x,
                    centered_text_top(self.rect.y0, self.heading_h, heading_line),
                ));
            text_renderer.draw_style(
                scene,
                &heading,
                tokens.colors.text_dim,
                heading_xf,
                None,
                &heading_style
                    .clone()
                    .with_weight(crate::text_render::TextWeight::Semibold),
            );
        }
        self.paint_heading_action(scene, transform, text_renderer, theme);
        let guide_color = crate::surface::edge_on(ground);
        let drag_source = self
            .reorder
            .is_active()
            .then(|| self.reorder.source().copied())
            .flatten();
        let body = Rect::new(self.rect.x0, self.body_top(), self.rect.x1, self.rect.y1);
        // A settled tree damages the rows that changed (a renamed or hovered
        // row, the rename field's caret), not its column. Rows in motion,
        // a drag, an open menu, delegated rows and navigation tiles prove
        // nothing, and repaint the tree.
        let tracks_rows = !self.is_nav()
            && self.mounted_rows.is_empty()
            && !self.transitions.is_active(now)
            && !self.reorder.is_active()
            && !self.reorder.is_tracking()
            && !self.menu.is_open();
        let mut row_parts = tracks_rows.then(Vec::new);
        let mut unproven = false;
        // What the tree drew before its body; a tracked tree opens the body's
        // clip when it puts its parts together, so each scene it keeps opens
        // and closes its own layers.
        let prefix = tracks_rows.then(|| std::mem::take(scene));
        let mut rows_scene = Scene::new();
        if !tracks_rows {
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &body,
            );
        }

        self.row_descriptions.clear();
        let visible = self.visible_row_range();
        'rows: for i in visible {
            // Where this row paints, once it is known: its part's bounds.
            let mut row_paint = None;
            'row: {
                let Some((key, depth)) = self.index.row_at(&self.model, i) else {
                    break 'row;
                };
                let (label, text_color, is_branch) = match key {
                    TreeRowKey::Node(id) => {
                        let Some(label) = self.model.label(id) else {
                            break 'row;
                        };
                        (label, tokens.colors.text_primary, self.model.is_branch(id))
                    }
                    TreeRowKey::Slot(slot) => match self.model.child_state(slot.parent, slot.index)
                    {
                        TreeChildState::Ready(_) => break 'row,
                        TreeChildState::Loading => {
                            ("Loading\u{2026}", tokens.colors.text_dim, false)
                        }
                        TreeChildState::Error { message } => (
                            message.unwrap_or("Could not load. Activate to retry"),
                            tokens.colors.semantic.danger,
                            false,
                        ),
                    },
                };
                let Some(view_top) = self.viewport.item_view_top(i) else {
                    break 'row;
                };
                let Some(row_height) = self.viewport.item_height(i) else {
                    break 'row;
                };
                let visual =
                    self.transitions
                        .visual(&key, RowGeometry::new(view_top, row_height), now);
                let row_height = visual.geometry.height;
                // The dragged row stays in place, faded, while its lifted copy
                // follows the pointer.
                let opacity = if matches!(key, TreeRowKey::Node(id) if drag_source == Some(id)) {
                    visual.opacity * 0.4
                } else {
                    visual.opacity
                };
                let body_top = self.rect.y0 + self.heading_h;
                let body_bottom = body_top + self.body_height();
                let y0 = body_top + visual.geometry.top;
                let y1 = (y0 + row_height).min(body_bottom);
                if y1 <= body_top {
                    break 'row;
                }
                if y0 >= body_bottom {
                    break 'rows;
                }
                let row_rect = Rect::new(self.rect.x0, y0, self.rect.x1, y1);
                row_paint = Some(row_rect);
                if let TreeRowKey::Node(id) = key
                    && self.is_nav()
                {
                    let faded = opacity < 1.0;
                    if faded {
                        scene.push_layer(
                            vello::peniko::Fill::NonZero,
                            vello::peniko::BlendMode::default(),
                            opacity,
                            transform,
                            &row_rect,
                        );
                    }
                    let painted =
                        self.draw_nav_row(scene, transform, text_renderer, theme, id, row_rect);
                    if faded {
                        scene.pop_layer();
                    }
                    if painted {
                        break 'row;
                    }
                }
                // The list's s2 inset, one hairline off each neighbour, at the
                // item radius: the same lift every list paints.
                let lift_rect = Rect::new(
                    row_rect.x0 + gap,
                    row_rect.y0 + hairline,
                    row_rect.x1 - gap,
                    row_rect.y1 - hairline,
                );
                let selected =
                    matches!(key, TreeRowKey::Node(id) if self.selection.is_selected(&id));
                let current = matches!(key, TreeRowKey::Node(id) if self.current == Some(id));
                let delegated = self.mounted_rows.iter().any(|row| row.key == key);
                let decoration = match (key, self.row_decoration.as_mut()) {
                    (TreeRowKey::Node(id), Some(decorate)) if !delegated => Some(decorate(
                        &self.model,
                        TreeRowInfo {
                            id,
                            depth,
                            is_branch,
                            expanded: self.expanded.contains(&id),
                            selected,
                            current,
                        },
                        theme,
                    )),
                    _ => None,
                };

                let hovered = self.hovered == Some(key) && !self.reorder.is_tracking();
                if selected || hovered {
                    let faded = opacity < 1.0;
                    if faded {
                        scene.push_layer(
                            vello::peniko::Fill::NonZero,
                            vello::peniko::BlendMode::default(),
                            opacity,
                            transform,
                            &row_rect,
                        );
                    }
                    crate::chrome::paint_row_selection(
                        scene,
                        transform,
                        lift_rect,
                        crate::chrome::SelectionState::new(selected, hovered, false),
                        ground,
                        theme,
                    );
                    if faded {
                        scene.pop_layer();
                    }
                }
                if let Some(key) = decoration
                    .as_ref()
                    .and_then(|decoration| decoration.presence.as_ref())
                {
                    // Mist is painted in device space.
                    let device = transform.transform_rect_bbox(row_rect);
                    for user in presence.peers_on(key) {
                        presence.draw_peer_mist(scene, theme, user, device);
                    }
                }
                self.paint_indent_guides(
                    scene,
                    transform,
                    row_rect,
                    depth,
                    guide_color.with_alpha_factor(opacity),
                );
                if self.is_focused && self.focused == Some(key) && crate::focus_visible::shown() {
                    let width = 2.0 * hairline;
                    let radius = f64::from(tokens.radii.item).min(lift_rect.height() / 2.0);
                    scene.stroke(
                        &Stroke::new(width),
                        transform,
                        tokens.colors.border_focus.with_alpha_factor(opacity),
                        None,
                        &vello::kurbo::RoundedRect::from_rect(
                            lift_rect.inset(-width / 2.0),
                            (radius - width / 2.0).max(0.0),
                        ),
                    );
                }

                let depth_px = f64_of_usize(depth) * indent_px;
                let glyph_x = self.rect.x0 + self.row_inset + depth_px;
                let mut label_x = glyph_x + self.disclosure_w;
                let text_y = centered_text_top(y0, row_height, line_h);

                if is_branch {
                    let TreeRowKey::Node(id) = key else {
                        break 'row;
                    };
                    paint_disclosure(
                        scene,
                        transform,
                        theme,
                        Rect::new(
                            glyph_x,
                            row_rect.y0,
                            glyph_x + self.disclosure_w,
                            row_rect.y1,
                        ),
                        self.expanded.contains(&id),
                        opacity,
                    );
                }

                if !delegated {
                    let actions_visible = self.row_actions_visible(key);
                    let mut text_color = text_color;
                    let mut label_max = self.rect.x1 - label_x - gap;
                    if let (TreeRowKey::Node(id), Some(description)) = (
                        key,
                        decoration
                            .as_ref()
                            .and_then(|decoration| decoration.description.clone()),
                    ) {
                        self.row_descriptions.insert(id, description);
                    }
                    if let Some(decoration) = &decoration {
                        if decoration.tone == TreeRowTone::Secondary {
                            text_color = tokens.colors.text_dim;
                        }
                        if let Some(icon) = &decoration.icon {
                            let used = paint_row_icon(
                                scene,
                                transform,
                                text_renderer,
                                theme,
                                icon,
                                Point::new(label_x, y0 + row_height / 2.0),
                                line_h,
                                opacity,
                            );
                            label_x += used + gap;
                            label_max -= used + gap;
                        }
                        if let Some(trailing) = &decoration.trailing {
                            let (trailing_w, _) =
                                text_renderer.measure_style(trailing, None, &text_style);
                            let trailing_x =
                                self.rect.x1 - f64::from(tokens.spacing.s3) - f64::from(trailing_w);
                            #[allow(
                                clippy::arithmetic_side_effects,
                                reason = "kurbo Affine composition is floating-point matrix math; it cannot overflow or panic"
                            )]
                            let trailing_xf = transform * Affine::translate((trailing_x, text_y));
                            text_renderer.draw_style(
                                scene,
                                trailing,
                                tokens.colors.text_dim.with_alpha_factor(opacity),
                                trailing_xf,
                                None,
                                &text_style,
                            );
                            label_max = trailing_x - gap - label_x;
                        }
                    }
                    if actions_visible {
                        self.paint_row_actions(scene, transform, theme, row_rect, ground);
                        if let Some(first) = self.row_action_rects(row_rect).first() {
                            label_max = label_max.min(first.x0 - gap - label_x);
                        }
                    }
                    if let (TreeRowKey::Node(id), Some(field)) = (key, self.rename.as_mut())
                        && field.id == id
                    {
                        // The field takes the label's place, at the label's size.
                        field.input.set_text_role(LABEL_ROLE);
                        field.input.apply_layout_external(Rect::new(
                            label_x,
                            y0,
                            (self.rect.x1 - gap).max(label_x),
                            y0 + row_height,
                        ));
                        break 'row;
                    }
                    // The current item is set in the heavier weight; elision is
                    // measured with it, so the label never jumps.
                    let strong = text_style.clone().strong();
                    let label = text_renderer.elide_to_width_style(
                        label,
                        f32_of_f64(label_max.max(0.0)),
                        &strong,
                    );
                    text_renderer.draw_style(
                        scene,
                        &label,
                        text_color.with_alpha_factor(opacity),
                        transform.pre_translate(vello::kurbo::Vec2::new(label_x, text_y)),
                        None,
                        if current { &strong } else { &text_style },
                    );
                }
            }
            if let Some(parts) = row_parts.as_mut() {
                let drawn = std::mem::take(scene);
                rows_scene.append(&drawn, None);
                match row_paint {
                    Some(rect) => parts.push(crate::widget::paint_damage::PaintPart::new(
                        i.to_string(),
                        rect,
                        drawn,
                        vec![rect],
                    )),
                    // Paint with no row bounds: nothing proves where it changed.
                    None if drawn.encoding().is_empty() => {}
                    None => unproven = true,
                }
            }
        }
        for row in &mut self.mounted_rows {
            let settled_top = self.viewport.item_view_top(row.index).unwrap_or_default();
            let settled_height = self
                .viewport
                .item_height(row.index)
                .unwrap_or_else(|| self.viewport.estimated_height());
            let visual = self.transitions.visual(
                &row.key,
                RowGeometry::new(settled_top, settled_height),
                now,
            );
            let delta = visual.geometry.top - settled_top;
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "kurbo Affine composition is finite screen-space matrix math"
            )]
            let row_transform = transform * Affine::translate((0.0, delta));
            scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                visual.opacity,
                transform,
                &self.rect,
            );
            row.widget
                .draw(scene, row_transform, text_renderer, theme, presence);
            scene.pop_layer();
        }
        for (_, payload, visual) in self.transitions.exiting(now) {
            if visual.opacity <= f32::EPSILON {
                continue;
            }
            let opacity = visual.opacity;
            let y0 = self.body_top() + visual.geometry.top;
            let row_rect = Rect::new(self.rect.x0, y0, self.rect.x1, y0 + visual.geometry.height);
            if payload.selected {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    opacity,
                    transform,
                    &row_rect,
                );
                crate::chrome::paint_row_selection(
                    scene,
                    transform,
                    Rect::new(
                        row_rect.x0 + gap,
                        row_rect.y0 + hairline,
                        row_rect.x1 - gap,
                        row_rect.y1 - hairline,
                    ),
                    crate::chrome::SelectionState::active(true),
                    ground,
                    theme,
                );
                scene.pop_layer();
            }
            let glyph_x = self.rect.x0 + self.row_inset + f64_of_usize(payload.depth) * indent_px;
            let text_y = centered_text_top(y0, visual.geometry.height, line_h);
            if payload.is_branch {
                paint_disclosure(
                    scene,
                    transform,
                    theme,
                    Rect::new(
                        glyph_x,
                        row_rect.y0,
                        glyph_x + self.disclosure_w,
                        row_rect.y1,
                    ),
                    payload.expanded,
                    opacity,
                );
            }
            let color = match payload.tone {
                TransitionTextTone::Primary => tokens.colors.text_primary,
                TransitionTextTone::Dim => tokens.colors.text_dim,
                TransitionTextTone::Danger => tokens.colors.semantic.danger,
            };
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "kurbo Affine composition is finite screen-space matrix math"
            )]
            let label_transform =
                transform * Affine::translate((glyph_x + self.disclosure_w, text_y));
            text_renderer.draw_style(
                scene,
                &payload.label,
                color.with_alpha_factor(opacity),
                label_transform,
                None,
                &text_style,
            );
        }
        self.place_nav_rename_field(text_renderer);
        self.rename_paint = None;
        let outer = tracks_rows.then(|| std::mem::take(scene));
        if let Some(field) = self.rename.as_mut() {
            field
                .input
                .draw(scene, transform, text_renderer, theme, presence);
            // The field, its focus ring and the correction chip it floats
            // past itself.
            let rect = field.input.get_rect();
            let reach = f64::from(tokens.spacing.s2);
            let ring = rect.inflate(reach, reach);
            self.rename_paint = Some(field.input.overhang().map_or(ring, |chip| ring.union(chip)));
        }
        if let (Some(outer), Some(parts)) = (outer, row_parts.as_mut()) {
            // The field is a part whether or not it is open, so opening and
            // closing it damage where it is and was, not the tree.
            let drawn = std::mem::replace(scene, outer);
            rows_scene.append(&drawn, None);
            parts.push(crate::widget::paint_damage::PaintPart::new(
                "rename".into(),
                self.rect,
                drawn,
                [self.rename_paint, self.drawn_rename_paint]
                    .into_iter()
                    .flatten()
                    .collect(),
            ));
        }
        if self.is_focused
            && self.affordance_focus_valid
            && let Some(target) = self.focused_affordance.as_deref()
            && let Some(button) = self.available_affordance(target)
            && let Some(bounds) = button.bounds
        {
            crate::widget::paint_focus_ring(
                scene,
                transform,
                bounds.to_rect(),
                f64::from(tokens.radii.detail),
                theme,
            );
        }
        // The lifted row floats beside the pointer; the drop line marks
        // where it lands, over the lifted row, so the copy never hides it.
        self.paint_drag_ghost(scene, transform, text_renderer, theme, &text_style, line_h);
        if let Some(preview) = self.reorder.preview() {
            let color = if self
                .reorder
                .target()
                .is_some_and(|target| target.placement == DropPlacement::Inside)
            {
                insertion.with_alpha_factor(0.25)
            } else {
                insertion
            };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                color,
                None,
                &preview,
            );
        }
        // What a tracked tree drew in its body after the rows.
        let inner = tracks_rows.then(|| std::mem::take(scene));
        if !tracks_rows {
            scene.pop_layer();
        }
        self.paint_edge_fades(scene, transform, ground);
        self.paint_scroll_thumb(scene, transform, theme);
        if let (Some(prefix), Some(inner)) = (prefix, inner) {
            let outer = std::mem::take(scene);
            let mut complete = prefix.clone();
            complete.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &body,
            );
            complete.append(&rows_scene, None);
            complete.append(&inner, None);
            complete.pop_layer();
            complete.append(&outer, None);
            *scene = complete;
            let frame = row_parts.filter(|_| !unproven).map(|parts| {
                // Everything but the rows, for telling whether it changed.
                let mut outside = prefix;
                outside.append(&inner, None);
                outside.append(&outer, None);
                crate::widget::paint_damage::PaintFrame::new(
                    vec![self.rect, body],
                    transform,
                    theme,
                    outside,
                    parts,
                )
            });
            self.paint_damage.record(self.rect, frame);
        } else {
            self.paint_damage.record(self.rect, None);
        }
        let own = std::mem::replace(scene, incoming);
        scene.append(&own, None);
    }

    /// The open context menu is a floating Panel in the window's top layer,
    /// above every pane and the window's chrome.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &crate::presence::PresenceCtx,
    ) {
        for row in &mut self.mounted_rows {
            row.widget
                .draw_overlay(scene, transform, text_renderer, theme, presence);
        }
        self.paint_context_menu(scene, transform, text_renderer, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.route_event(event, ctx);
        // Pointer motion repaints only through what the view signature sees
        // (hover, affordance hover, drags), so a resting pointer stays inert.
        // Other input the tree acted on, or could have, repaints it.
        let motion = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::CursorLeft { .. }
                | WindowEvent::CursorEntered { .. }
        );
        if !motion
            && (*ctx.handled
                || ctx.focus_target.is_some()
                || self.is_focused
                || self
                    .last_cursor
                    .is_some_and(|point| self.rect.contains(point)))
        {
            self.paint_dirty = true;
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.mounted_rows
            .iter()
            .map(|row| row.widget.as_ref())
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for row in &self.mounted_rows {
            visitor(row.widget.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.mounted_rows
            .iter_mut()
            .map(|row| row.widget.as_mut() as &mut dyn Widget)
            .collect()
    }

    fn accessibility_child_parent(&self, child_index: usize) -> Option<usize> {
        let row = self.mounted_rows.get(child_index)?;
        let visible = self.visible_row_range();
        let absolute = self.index.index_of_key(&self.model, row.key)?;
        // Tree items follow the heading action's button when there is one.
        let offset = usize::from(self.heading_action_node().is_some());
        visible.contains(&absolute).then(|| {
            absolute
                .saturating_sub(visible.start)
                .saturating_add(offset)
        })
    }

    fn wants_pointer_capture(&self) -> bool {
        self.reorder.is_tracking()
            || self.drag_scroll.is_tracking()
            || self.menu.is_open()
            || self
                .mounted_rows
                .iter()
                .any(|row| row.widget.wants_pointer_capture())
    }

    fn needs_redraw(&self) -> bool {
        self.paint_dirty
            || self.drawn_signature != Some(self.paint_signature())
            || self.momentum.is_active()
            || self.reorder.is_active()
            || self.transitions.is_active(crate::time::now())
            || self.menu.is_open() && self.menu.needs_redraw()
            || self
                .rename
                .as_ref()
                .is_some_and(|field| field.input.needs_redraw())
            || self
                .mounted_rows
                .iter()
                .any(|row| row.widget.needs_redraw())
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() || self.paint_damage.is_recorded() {
            self.paint_damage.collect(self.rect, regions);
        }
    }

    fn release_scene_cache(&mut self) {
        self.paint_damage.release_scene_cache();
        for row in &mut self.mounted_rows {
            row.widget.release_scene_cache();
        }
    }

    fn mark_drawn(&mut self) {
        self.paint_dirty = false;
        self.drawn_signature = Some(self.paint_signature());
        if self.paint_damage.is_recorded() {
            self.drawn_rename_paint = self.rename_paint;
        }
        self.paint_damage.mark_drawn();
        if let Some(field) = self.rename.as_mut() {
            field.input.mark_drawn();
        }
        if self.menu.is_open() {
            self.menu.mark_drawn();
        }
        for row in &mut self.mounted_rows {
            row.widget.mark_drawn();
        }
    }

    fn needs_animation_frame(&self) -> bool {
        self.bar_fade
            .get()
            .fading_at(crate::motion::frame_now(), self.bar_held.get())
            || self.momentum.is_active()
            || self.model.needs_poll()
            || self.reorder.is_active()
            || self.transitions.is_active(crate::time::now())
            || self
                .mounted_rows
                .iter()
                .any(|row| row.widget.needs_animation_frame())
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        let mut earliest = self
            .bar_fade
            .get()
            .fade_deadline_at(crate::motion::frame_now(), self.bar_held.get());
        for row in &self.mounted_rows {
            if let Some(deadline) = row.widget.next_wake_deadline() {
                earliest = Some(earliest.map_or(deadline, |old| old.min(deadline)));
            }
        }
        earliest
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.delegate_structure_dirty);
        for row in &mut self.mounted_rows {
            dirty |= row.widget.take_structure_dirty();
        }
        dirty
    }

    fn is_layout_dirty(&self) -> bool {
        self.delegate_layout_dirty
            || self
                .mounted_rows
                .iter()
                .any(|row| row.widget.is_layout_dirty())
    }

    fn layout_needs_measure(&self) -> bool {
        self.delegate_layout_dirty
            || self
                .mounted_rows
                .iter()
                .any(|row| row.widget.layout_needs_measure())
    }

    fn name(&self) -> &'static str {
        "TreeView"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, focused: bool) {
        self.is_focused = focused;
        // Focus leaving the tree ends an inline rename, keeping the edit.
        if !focused {
            self.focused_affordance = None;
            self.commit_rename();
        }
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.accessibility_shared()
            .map(crate::a11y::SharedAccessNode::into_owned)
    }

    /// A tree is never hidden: answered without building its rows.
    fn accessibility_hidden(&self) -> bool {
        false
    }

    /// The tree, its rows shared with the last walk where nothing they are
    /// built from changed ([`Self::access_rows`]): a walk while the tree
    /// is unchanged (a keystroke in the editor beside it) builds no row.
    fn accessibility_shared(&self) -> Option<crate::a11y::SharedAccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        use std::sync::Arc;
        // Labels are runtime strings supplied by the host, not FTL keys.
        let value = self
            .selected()
            .and_then(|id| self.model.label(id))
            .map(str::to_owned);
        let mut node = AccessNode::new(Role::Tree)
            .with_optional_name(
                self.accessible_name
                    .as_deref()
                    .map(crate::i18n::I18nManager::lookup_plain)
                    .or_else(|| self.heading.clone()),
            )
            .with_optional_value(value)
            .with_state(
                AccessState::new()
                    .focusable(self.is_focusable())
                    .focused(self.is_focused())
                    .disabled(!self.is_enabled()),
            );
        if let Some(sel) = self.selected()
            && let Some(idx) = self.visible_index_of(sel)
            && let Some((TreeRowKey::Node(_), depth)) = self.index.row_at(&self.model, idx)
        {
            node = node.with_position(idx.saturating_add(1), self.index.len());
            node = node.with_level(depth.saturating_add(1));
        }
        // Keep the semantic tree bounded exactly like painting. Absolute
        // positions retain useful "N of M" announcements even though rows
        // outside the viewport have no synthetic node this frame.
        let rows = self.access_rows();
        let mut children = Vec::with_capacity(rows.len().saturating_add(2));
        children.extend(self.heading_action_node().map(Arc::new));
        children.extend(rows);
        children.extend(self.menu_node().map(Arc::new));
        Some(crate::a11y::SharedAccessNode { node, children })
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let mut node = self.accessibility()?;
        for index in path {
            node = node.children.into_iter().nth(*index)?;
        }
        node.bounds.map(crate::a11y::AccessBounds::to_rect)
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else {
            return false;
        };
        let offset = usize::from(self.heading_action_node().is_some());
        let Some(row) = index.checked_sub(offset) else {
            return false;
        };
        let absolute = self.visible_row_range().start.saturating_add(row);
        if !self.visible_row_range().contains(&absolute) || !self.focus_at(absolute) {
            return false;
        }
        self.is_focused = true;
        self.follow_focus();
        true
    }

    fn tooltip_target_at(&self, point: Point) -> Option<(Rect, String)> {
        self.affordance_tooltip(point)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.rename.is_some() || self.menu.is_open() {
            return None;
        }
        let on_heading_action = self
            .heading_action_rect()
            .is_some_and(|rect| rect.contains(point));
        (on_heading_action || self.row_key_at(point).is_some())
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let (state, path) = crate::doc::document(state, path);

        self.paint_dirty = true;
        if let Some(performed) = self.perform_affordance_action(target, action, state, path) {
            return performed;
        }
        if let Some(raw) = target.strip_prefix("node:") {
            let Ok(id) = raw.parse::<NodeId>() else {
                return false;
            };
            if !self.model.contains(id) {
                return false;
            }
            match action {
                Action::Focus => {
                    self.focused_affordance = None;
                    let Some(index) = self.visible_index_of(id) else {
                        return false;
                    };
                    self.focus_at(index)
                }
                Action::Activate => {
                    self.focused_affordance = None;
                    self.focused = Some(TreeRowKey::Node(id));
                    self.activate_focus(true)
                }
                Action::Expand => {
                    if !self.model.is_branch(id) {
                        return false;
                    }
                    let _ = self.set_expanded(id, true);
                    true
                }
                Action::Collapse => {
                    if !self.model.is_branch(id) {
                        return false;
                    }
                    let _ = self.set_expanded(id, false);
                    true
                }
                Action::ScrollIntoView => {
                    if self.visible_index_of(id).is_none() {
                        return false;
                    }
                    self.reveal(id);
                    true
                }
                Action::ShowContextMenu => {
                    let at = self
                        .row_rect(id)
                        .map_or(Point::new(self.rect.x0, self.body_top()), |row| {
                            Point::new(row.x0 + f64::from(self.spacing.s6), row.y1)
                        });
                    self.open_row_menu(id, at)
                }
                _ => false,
            }
        } else if let Some(raw) = target.strip_prefix("slot:") {
            let Some((parent, index)) = raw.split_once(':') else {
                return false;
            };
            let (Ok(parent), Ok(index)) = (parent.parse::<NodeId>(), index.parse::<usize>()) else {
                return false;
            };
            let slot = TreeSlot { parent, index };
            match action {
                Action::Focus => self
                    .index
                    .index_of_key(&self.model, TreeRowKey::Slot(slot))
                    .is_some_and(|index| self.focus_at(index)),
                Action::Activate => {
                    self.focused_affordance = None;
                    self.focused = Some(TreeRowKey::Slot(slot));
                    self.activate_focus(true)
                }
                _ => false,
            }
        } else {
            false
        }
    }
}

impl<T: 'static, M: TreeModel + 'static> TreeView<T, M> {
    #[allow(
        clippy::too_many_lines,
        reason = "keeps delegate, reorder, disclosure, selection, scroll, and keyboard precedence explicit"
    )]
    fn route_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let scale_factor = ctx.scale_factor;
        let modifiers = ctx.modifiers;
        let state = &mut ctx.state;
        let path = ctx.path;
        let handled = &mut *ctx.handled;
        let focus_target = &mut *ctx.focus_target;
        let node_id = ctx.node_id;
        let taffy = ctx.taffy;
        if let WindowEvent::CursorMoved { position, .. } = event {
            let point = Point::new(position.x / scale_factor, position.y / scale_factor);
            self.last_cursor = Some(point);
            self.hovered = self.row_key_at(point);
        }
        if let WindowEvent::CursorLeft { .. } = event {
            self.hovered = None;
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        ) {
            self.focused_affordance = None;
        }
        if let WindowEvent::KeyboardInput { event, .. } = event
            && event.state == ElementState::Pressed
            && self.is_focused
            && self.focused_affordance.is_some()
            && (event.logical_key == Key::Named(NamedKey::Enter)
                || event.logical_key == Key::Named(NamedKey::Space)
                || matches!(&event.logical_key, Key::Character(text) if text == " "))
        {
            let target = self.focused_affordance.clone().unwrap_or_default();
            if self.affordance_focus_valid && self.available_affordance(&target).is_some() {
                let _ = self.perform_affordance_action(
                    &target,
                    &crate::a11y::Action::Activate,
                    state,
                    path,
                );
            } else {
                self.focused_affordance = None;
            }
            // A stale button must not fall through to activate its parent row.
            *handled = true;
            return;
        }
        if self.route_context_menu(
            event,
            &mut crate::widget::EventCtx {
                scale_factor,
                modifiers,
                state: state.reborrow(),
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            },
        ) {
            return;
        }
        if self.route_rename(
            event,
            &mut crate::widget::EventCtx {
                scale_factor,
                modifiers,
                state: state.reborrow(),
                path,
                handled,
                focus_target,
                node_id,
                taffy,
            },
        ) {
            return;
        }
        if !self.reorder.is_tracking() && !self.drag_scroll.is_tracking() {
            crate::widget::route_to_mounted(
                &mut self.mounted_rows,
                |mounted| (&mut mounted.widget, mounted.node),
                event,
                &mut EventCtx {
                    scale_factor,
                    modifiers,
                    state: state.reborrow(),
                    path,
                    handled,
                    focus_target,
                    node_id,
                    taffy,
                },
            );
        }
        if *handled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { .. } if self.reorder.is_tracking() => {
                if let Some(cursor) = self.last_cursor {
                    let now = crate::time::now();
                    self.update_reorder_pointer(cursor, now);
                    self.last_tick = Some(now);
                    *handled = self.reorder.is_active();
                }
            }
            WindowEvent::CursorMoved { .. } if self.drag_scroll.is_tracking() => {
                if let Some(cursor) = self.last_cursor {
                    let now = crate::time::now();
                    if let Some(update) = self.drag_scroll.update(cursor.y, now) {
                        self.momentum.stop();
                        self.set_scroll_offset(update.target_offset);
                        self.last_tick = Some(now);
                        *handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.momentum.stop();
                if let Some(cursor) = self.last_cursor
                    && self.rect.contains(cursor)
                    && self.press_affordance(cursor)
                {
                    *focus_target = Some(node_id);
                    *handled = true;
                    return;
                }
                let reorderable = self.reorder_validator.is_some() && self.reorder_commit.is_some();
                let source = reorderable
                    .then(|| self.reorder_source_at_cursor())
                    .flatten();
                let toggle = modifiers.state().control_key() || modifiers.state().super_key();
                let extend = modifiers.state().shift_key();
                if reorderable && self.disclosure_at_cursor() {
                    // Rows of a reorderable tree act on press, so its
                    // disclosure does too.
                    *handled = self.on_left_press(node_id, focus_target, toggle, extend);
                } else if let (Some(source), Some(cursor)) = (source, self.last_cursor) {
                    self.on_left_press(node_id, focus_target, toggle, extend);
                    // A double click that opened the rename field is not
                    // the start of a drag.
                    if self.rename.is_none() {
                        self.reorder.begin(source, cursor);
                        self.last_tick = Some(crate::time::now());
                    }
                    *handled = true;
                } else if let Some(cursor) = self.last_cursor
                    && self.rect.contains(cursor)
                {
                    self.drag_scroll
                        .begin(cursor.y, self.scroll_offset(), crate::time::now());
                    *focus_target = Some(node_id);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.context_menu_items.is_some() || self.nav_offers_menu() => {
                if let Some(cursor) = self.last_cursor
                    && self.rect.contains(cursor)
                {
                    self.reorder.cancel();
                    self.drag_scroll.cancel();
                    match self.row_key_at(cursor) {
                        Some(TreeRowKey::Node(id)) => {
                            self.open_row_menu(id, cursor);
                        }
                        _ => {
                            self.open_context_menu(None, cursor);
                        }
                    }
                    *focus_target = Some(node_id);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.reorder.is_tracking() => {
                if let Some(dropped) = self.reorder.finish() {
                    self.commit_reorder(dropped);
                }
                *handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.drag_scroll.is_tracking() => {
                if let Some(drag) = self.drag_scroll.finish(crate::time::now()) {
                    if drag.dragged {
                        if self.reduce_motion {
                            self.momentum.stop();
                        } else {
                            self.momentum.fling(drag.velocity);
                            self.last_tick = Some(crate::time::now());
                        }
                    } else {
                        let toggle =
                            modifiers.state().control_key() || modifiers.state().super_key();
                        let extend = modifiers.state().shift_key();
                        self.on_left_press(node_id, focus_target, toggle, extend);
                    }
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, phase, .. }
                if self
                    .last_cursor
                    .is_some_and(|point| self.rect.contains(point)) =>
            {
                self.drag_scroll.cancel();
                *handled = self.on_wheel(delta, *phase);
                // It moved only this tree's view: nothing the app publishes.
                if *handled {
                    crate::widget::note_view_only_pan();
                }
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } if self.is_focused => {
                self.momentum.stop();
                let mods = modifiers.state();
                let menu_key = matches!(logical_key, Key::Named(NamedKey::ContextMenu))
                    || (mods.shift_key() && matches!(logical_key, Key::Named(NamedKey::F10)));
                if *logical_key == Key::Named(NamedKey::Escape)
                    && (self.reorder.cancel() || self.drag_scroll.cancel())
                {
                    *handled = true;
                } else if menu_key && (self.context_menu_items.is_some() || self.nav_offers_menu())
                {
                    *handled = self.open_menu_from_keyboard();
                } else if *logical_key == Key::Named(NamedKey::F2) && self.on_rename.is_some() {
                    *handled = self.command_node().is_some_and(|id| self.begin_rename(id));
                } else if *logical_key == Key::Named(NamedKey::Delete) && self.on_delete.is_some() {
                    if let (Some(id), Some(callback)) = (self.command_node(), &mut self.on_delete) {
                        callback(id);
                        *handled = true;
                    }
                } else if let Some(movement) = self.key_move(logical_key, mods) {
                    *handled = self
                        .command_node()
                        .is_some_and(|id| self.move_node(id, movement));
                } else {
                    let toggle = modifiers.state().control_key() || modifiers.state().super_key();
                    let extend = modifiers.state().shift_key();
                    *handled = self.on_key_with_modifiers(logical_key, toggle, extend);
                }
            }
            _ => {}
        }
    }

    /// A digest of the view state the paint pass reads outside the model:
    /// geometry, scroll, expansion, selection, focus, hover and the pointer
    /// affordances. Model content changes set `paint_dirty` instead.
    fn paint_signature(&self) -> u64 {
        use std::hash::{BuildHasher, Hash, Hasher};

        // Unordered sets fold with a commutative sum so iteration order does
        // not change the digest.
        let hasher =
            std::hash::BuildHasherDefault::<std::collections::hash_map::DefaultHasher>::default();
        let unordered = |keys: &mut dyn Iterator<Item = NodeId>| {
            keys.fold(0_u64, |sum, key| sum.wrapping_add(hasher.hash_one(key)))
        };
        let mut state = hasher.build_hasher();
        for edge in [self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y1] {
            edge.to_bits().hash(&mut state);
        }
        self.viewport.offset().to_bits().hash(&mut state);
        self.viewport.viewport().to_bits().hash(&mut state);
        self.viewport.content_extent().to_bits().hash(&mut state);
        self.index.len().hash(&mut state);
        unordered(&mut self.expanded.iter().copied()).hash(&mut state);
        unordered(&mut self.selection.stored_keys().copied()).hash(&mut state);
        self.selection.is_select_all().hash(&mut state);
        self.selection.primary().hash(&mut state);
        self.selection.anchor().hash(&mut state);
        self.focused.hash(&mut state);
        self.current.hash(&mut state);
        self.hovered.hash(&mut state);
        self.is_focused.hash(&mut state);
        self.heading.hash(&mut state);
        self.rename.as_ref().map(|field| field.id).hash(&mut state);
        self.menu.is_open().hash(&mut state);
        self.menu_target.hash(&mut state);
        self.reorder.is_tracking().hash(&mut state);
        self.reorder.is_active().hash(&mut state);
        self.drag_scroll.is_tracking().hash(&mut state);
        self.momentum.is_active().hash(&mut state);
        self.transitions
            .is_active(crate::time::now())
            .hash(&mut state);
        // Pointer-driven chrome: the heading action and the hovered row's
        // actions lift under the pointer.
        let cursor = self.last_cursor;
        let over = |rect: Rect| cursor.is_some_and(|point| rect.contains(point));
        self.heading_action_rect()
            .is_some_and(over)
            .hash(&mut state);
        match self.hovered {
            Some(TreeRowKey::Node(id)) if self.row_actions_visible(TreeRowKey::Node(id)) => self
                .row_rect(id)
                .and_then(|row| self.row_action_rects(row).into_iter().position(over)),
            _ => None,
        }
        .hash(&mut state);
        // While a drag, menu or rename follows the pointer, every move shows.
        if self.reorder.is_tracking() || self.menu.is_open() || self.rename.is_some() {
            cursor
                .map(|point| (point.x.to_bits(), point.y.to_bits()))
                .hash(&mut state);
        }
        state.finish()
    }
}

/// Paint a decorated row's leading icon centred vertically on `anchor.y`,
/// starting at `anchor.x`. Returns the width it occupied.
fn paint_row_icon(
    scene: &mut Scene,
    transform: Affine,
    text_renderer: &mut TextRenderer,
    theme: &Theme,
    icon: &TreeRowIcon,
    anchor: Point,
    line_h: f32,
    opacity: f32,
) -> f64 {
    let tokens = &theme.tokens;
    let side = f64::from(tokens.spacing.s4).min(f64::from(line_h)).max(1.0);
    match icon {
        TreeRowIcon::Icon { icon, color } => {
            let rect = Rect::new(
                anchor.x,
                anchor.y - side / 2.0,
                anchor.x + side,
                anchor.y + side / 2.0,
            );
            icon.draw(scene, transform, rect, color.with_alpha_factor(opacity));
            side
        }
        TreeRowIcon::Glyph { text, color } => {
            let style = tokens.typography.style(TextRole::Caption);
            let (w, h) = text_renderer.measure_style(text, None, &style);
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "kurbo Affine composition is floating-point matrix math; it cannot overflow or panic"
            )]
            let xf = transform * Affine::translate((anchor.x, anchor.y - f64::from(h) / 2.0));
            text_renderer.draw_style(
                scene,
                text,
                color.with_alpha_factor(opacity),
                xf,
                None,
                &style,
            );
            f64::from(w).max(side)
        }
        TreeRowIcon::Badge { text, fill } => {
            let style = tokens.typography.style(TextRole::Caption);
            let (w, h) = text_renderer.measure_style(text, None, &style);
            let chip_w = side.max(f64::from(w) + f64::from(tokens.spacing.s1));
            let chip = Rect::new(
                anchor.x,
                anchor.y - side / 2.0,
                anchor.x + chip_w,
                anchor.y + side / 2.0,
            );
            let radius = f64::from(tokens.radii.detail);
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                fill.with_alpha_factor(opacity),
                None,
                &chip.to_rounded_rect(radius),
            );
            let ink = tokens.colors.on_surface(*fill).ink;
            #[allow(
                clippy::arithmetic_side_effects,
                reason = "kurbo Affine composition is floating-point matrix math; it cannot overflow or panic"
            )]
            let xf = transform
                * Affine::translate((
                    chip.x0 + (chip_w - f64::from(w)) / 2.0,
                    anchor.y - f64::from(h) / 2.0,
                ));
            text_renderer.draw_style(
                scene,
                text,
                ink.with_alpha_factor(opacity),
                xf,
                None,
                &style,
            );
            chip_w
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Dispatch, dispatch};
    use automerge::AutoCommit;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use taffy::TaffyTree;
    use taffy::prelude::TaffyMaxContent;

    fn sample_tree() -> TreeNode<()> {
        TreeNode::branch(
            1,
            "root",
            (),
            vec![
                TreeNode::leaf(2, "child1", ()),
                TreeNode::leaf(3, "child2", ()),
            ],
        )
    }

    fn reorder_tree() -> TreeNode<&'static str> {
        TreeNode::branch(
            1,
            "root",
            "root-data",
            vec![
                TreeNode::branch(
                    2,
                    "source folder",
                    "folder-data",
                    vec![
                        TreeNode::leaf(3, "first", "first-data"),
                        TreeNode::branch(
                            4,
                            "nested",
                            "nested-data",
                            vec![TreeNode::leaf(5, "nested leaf", "leaf-data")],
                        ),
                    ],
                ),
                TreeNode::branch(6, "destination", "destination-data", Vec::new()),
                TreeNode::leaf(7, "last", "last-data"),
            ],
        )
    }

    #[test]
    fn owned_tree_reorder_moves_subtrees_rejects_invalid_targets_and_commits_drops() {
        // Case `owned_tree_reorder_moves_complete_subtrees_across_parents`.
        {
            let mut tree = reorder_tree();
            assert!(tree.reorder(&ReorderDrop {
                source: 4,
                target: DropTarget::new(6, DropPlacement::Inside),
            }));
            assert_eq!(tree.parent_id(4), Some(6));
            assert_eq!(tree.child_id(2, 0), Some(3));
            assert_eq!(tree.child_id(6, 0), Some(4));
            assert_eq!(tree.find(5).map(|node| node.data), Some("leaf-data"));

            assert!(tree.reorder(&ReorderDrop {
                source: 3,
                target: DropTarget::new(7, DropPlacement::After),
            }));
            assert_eq!(tree.parent_id(3), Some(1));
            assert_eq!(
                tree.children.iter().map(|node| node.id).collect::<Vec<_>>(),
                vec![2, 6, 7, 3]
            );
            assert_eq!(tree.find(3).map(|node| node.data), Some("first-data"));
        }

        // Case `owned_tree_reorder_rejects_cycles_invalid_targets_and_noops`.
        {
            let mut tree = reorder_tree();
            let original = tree.children.iter().map(|node| node.id).collect::<Vec<_>>();
            for dropped in [
                ReorderDrop {
                    source: 1,
                    target: DropTarget::new(2, DropPlacement::Before),
                },
                ReorderDrop {
                    source: 2,
                    target: DropTarget::new(4, DropPlacement::Inside),
                },
                ReorderDrop {
                    source: 3,
                    target: DropTarget::new(7, DropPlacement::Inside),
                },
                ReorderDrop {
                    source: 99,
                    target: DropTarget::new(7, DropPlacement::After),
                },
                ReorderDrop {
                    source: 3,
                    target: DropTarget::new(4, DropPlacement::Before),
                },
                ReorderDrop {
                    source: 5,
                    target: DropTarget::new(4, DropPlacement::Inside),
                },
            ] {
                assert!(!tree.reorder(&dropped));
            }
            assert_eq!(
                tree.children.iter().map(|node| node.id).collect::<Vec<_>>(),
                original
            );
            assert_eq!(tree.parent_id(5), Some(4));
        }

        // Case `owned_tree_reorderable_installs_safe_default_controller`.
        {
            let view = TreeView::new(reorder_tree()).reorderable();
            assert!(view.reorder_validator.is_some());
            assert!(view.reorder_commit.is_some());
        }

        // Case `owned_tree_reorderable_commits_the_pointer_drop_without_app_bookkeeping`.
        {
            let mut view = TreeView::new(sample_tree())
                .with_expanded([1])
                .reorderable();
            view.set_viewport(88.0, f32_of_f64(ROW_HEIGHT));
            let (taffy, node) = lay_out_tree(&mut view, 240.0);

            dispatch_laid_out(&mut view, &cursor_moved(80.0, 33.0), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
            dispatch_laid_out(&mut view, &cursor_moved(80.0, 65.0), &taffy, node);
            assert_eq!(
                view.reorder_target(),
                Some(&DropTarget::new(3, DropPlacement::After))
            );
            dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);

            assert_eq!(
                view.model()
                    .children
                    .iter()
                    .map(|node| node.id)
                    .collect::<Vec<_>>(),
                vec![3, 2]
            );
            assert_eq!(view.visible_index_of(2), Some(2));
        }
    }

    struct ExternalNode {
        label: String,
        branch: bool,
        children: Vec<NodeId>,
    }

    struct ExternalModel {
        root: NodeId,
        nodes: HashMap<NodeId, ExternalNode>,
        child_queries: Cell<usize>,
    }

    impl TreeModel for ExternalModel {
        fn root_id(&self) -> NodeId {
            self.root
        }

        fn contains(&self, id: NodeId) -> bool {
            self.nodes.contains_key(&id)
        }

        fn label(&self, id: NodeId) -> Option<&str> {
            self.nodes.get(&id).map(|node| node.label.as_str())
        }

        fn is_branch(&self, id: NodeId) -> bool {
            self.nodes.get(&id).is_some_and(|node| node.branch)
        }

        fn parent_id(&self, id: NodeId) -> Option<NodeId> {
            self.nodes
                .iter()
                .find_map(|(parent, node)| node.children.contains(&id).then_some(*parent))
        }

        fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
            self.nodes
                .get(&parent)?
                .children
                .iter()
                .position(|id| *id == child)
        }

        fn child_count(&self, parent: NodeId) -> usize {
            self.child_queries
                .set(self.child_queries.get().saturating_add(1));
            self.nodes
                .get(&parent)
                .map_or(0, |node| node.children.len())
        }

        fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
            self.nodes.get(&parent)?.children.get(index).copied()
        }
    }

    fn external_model() -> ExternalModel {
        ExternalModel {
            root: 1,
            nodes: HashMap::from([
                (
                    1,
                    ExternalNode {
                        label: "root".into(),
                        branch: true,
                        children: vec![2, 3, 4],
                    },
                ),
                (
                    2,
                    ExternalNode {
                        label: "folder".into(),
                        branch: true,
                        children: vec![20],
                    },
                ),
                (
                    20,
                    ExternalNode {
                        label: "hidden child".into(),
                        branch: false,
                        children: Vec::new(),
                    },
                ),
                (
                    3,
                    ExternalNode {
                        label: "anchor".into(),
                        branch: false,
                        children: Vec::new(),
                    },
                ),
                (
                    4,
                    ExternalNode {
                        label: "last".into(),
                        branch: false,
                        children: Vec::new(),
                    },
                ),
            ]),
            child_queries: Cell::new(0),
        }
    }

    struct SharedOrderModel {
        children: Rc<RefCell<Vec<NodeId>>>,
    }

    impl TreeModel for SharedOrderModel {
        fn root_id(&self) -> NodeId {
            1
        }

        fn contains(&self, id: NodeId) -> bool {
            id == 1 || self.children.borrow().contains(&id)
        }

        fn label(&self, id: NodeId) -> Option<&str> {
            self.contains(id).then_some("row")
        }

        fn is_branch(&self, id: NodeId) -> bool {
            id == 1
        }

        fn parent_id(&self, id: NodeId) -> Option<NodeId> {
            (id != 1 && self.contains(id)).then_some(1)
        }

        fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
            (parent == 1)
                .then(|| self.children.borrow().iter().position(|id| *id == child))
                .flatten()
        }

        fn child_count(&self, parent: NodeId) -> usize {
            if parent == 1 {
                self.children.borrow().len()
            } else {
                0
            }
        }

        fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
            (parent == 1)
                .then(|| self.children.borrow().get(index).copied())
                .flatten()
        }
    }

    #[derive(Clone, Copy)]
    enum PagedSlotState {
        Loading,
        Error,
        Ready,
    }

    const PAGED_ID_BASE: NodeId = 20_000_000_000;

    struct PagedModel {
        count: usize,
        states: Rc<RefCell<HashMap<usize, PagedSlotState>>>,
        self_loading: bool,
        requests: Vec<TreeLoadRequest>,
        changes: Vec<TreeModelChange>,
    }

    impl PagedModel {
        fn id(index: usize) -> Option<NodeId> {
            Some(PAGED_ID_BASE.saturating_add(u64::try_from(index).ok()?))
        }

        fn index(id: NodeId) -> Option<usize> {
            usize::try_from(id.checked_sub(PAGED_ID_BASE)?).ok()
        }

        fn state(&self, index: usize) -> Option<PagedSlotState> {
            (index < self.count).then(|| {
                self.states
                    .borrow()
                    .get(&index)
                    .copied()
                    .unwrap_or(PagedSlotState::Loading)
            })
        }
    }

    impl TreeModel for PagedModel {
        fn root_id(&self) -> NodeId {
            1
        }

        fn contains(&self, id: NodeId) -> bool {
            if id == 1 {
                return true;
            }
            Self::index(id)
                .is_some_and(|index| matches!(self.state(index), Some(PagedSlotState::Ready)))
        }

        fn label(&self, id: NodeId) -> Option<&str> {
            if id == 1 {
                Some("root")
            } else {
                self.contains(id).then_some("loaded row")
            }
        }

        fn is_branch(&self, id: NodeId) -> bool {
            id == 1
        }

        fn parent_id(&self, id: NodeId) -> Option<NodeId> {
            (id != 1 && self.contains(id)).then_some(1)
        }

        fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
            (parent == 1)
                .then(|| Self::index(child))
                .flatten()
                .filter(|index| matches!(self.state(*index), Some(PagedSlotState::Ready)))
        }

        fn child_count(&self, parent: NodeId) -> usize {
            if parent == 1 { self.count } else { 0 }
        }

        fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
            (parent == 1 && matches!(self.state(index), Some(PagedSlotState::Ready)))
                .then(|| Self::id(index))
                .flatten()
        }

        fn child_state(&self, parent: NodeId, index: usize) -> TreeChildState<'_> {
            if parent != 1 {
                return TreeChildState::Error {
                    message: Some("Unknown parent"),
                };
            }
            match self.state(index) {
                Some(PagedSlotState::Loading) => TreeChildState::Loading,
                Some(PagedSlotState::Error) => TreeChildState::Error {
                    message: Some("Network unavailable"),
                },
                Some(PagedSlotState::Ready) => Self::id(index).map_or(
                    TreeChildState::Error {
                        message: Some("Invalid row id"),
                    },
                    TreeChildState::Ready,
                ),
                None => TreeChildState::Error {
                    message: Some("Row out of bounds"),
                },
            }
        }

        fn request_load(&mut self, request: TreeLoadRequest) -> bool {
            if !self.self_loading || request.slot.parent != 1 || request.slot.index >= self.count {
                return false;
            }
            self.requests.push(request);
            self.states
                .borrow_mut()
                .insert(request.slot.index, PagedSlotState::Ready);
            self.changes.push(TreeModelChange::SlotsChanged {
                slots: vec![request.slot],
            });
            true
        }

        fn poll_changes(&mut self) -> Vec<TreeModelChange> {
            std::mem::take(&mut self.changes)
        }
    }

    const HUGE_CHILDREN: usize = 100_000_000;
    const HUGE_BRANCH_POSITION: usize = 50_000_000;
    const HUGE_CHILD_BASE: NodeId = 1_000_000_000;
    const HUGE_GRANDCHILD_A: NodeId = 9_000_000_001;
    const HUGE_GRANDCHILD_B: NodeId = 9_000_000_002;

    struct HugeModel {
        child_id_queries: Cell<usize>,
    }

    impl HugeModel {
        fn direct_id(index: usize) -> Option<NodeId> {
            Some(HUGE_CHILD_BASE.saturating_add(u64::try_from(index).ok()?))
        }

        fn direct_index(id: NodeId) -> Option<usize> {
            let offset = id.checked_sub(HUGE_CHILD_BASE)?;
            usize::try_from(offset)
                .ok()
                .filter(|index| *index < HUGE_CHILDREN)
        }

        fn branch_id() -> NodeId {
            Self::direct_id(HUGE_BRANCH_POSITION).unwrap()
        }
    }

    impl TreeModel for HugeModel {
        fn root_id(&self) -> NodeId {
            1
        }

        fn contains(&self, id: NodeId) -> bool {
            id == 1
                || Self::direct_index(id).is_some()
                || matches!(id, HUGE_GRANDCHILD_A | HUGE_GRANDCHILD_B)
        }

        fn label(&self, id: NodeId) -> Option<&str> {
            self.contains(id).then_some("row")
        }

        fn is_branch(&self, id: NodeId) -> bool {
            id == 1 || id == Self::branch_id()
        }

        fn parent_id(&self, id: NodeId) -> Option<NodeId> {
            if Self::direct_index(id).is_some() {
                Some(1)
            } else if matches!(id, HUGE_GRANDCHILD_A | HUGE_GRANDCHILD_B) {
                Some(Self::branch_id())
            } else {
                None
            }
        }

        fn child_index(&self, parent: NodeId, child: NodeId) -> Option<usize> {
            if parent == 1 {
                Self::direct_index(child)
            } else if parent == Self::branch_id() {
                match child {
                    HUGE_GRANDCHILD_A => Some(0),
                    HUGE_GRANDCHILD_B => Some(1),
                    _ => None,
                }
            } else {
                None
            }
        }

        fn child_count(&self, parent: NodeId) -> usize {
            if parent == 1 {
                HUGE_CHILDREN
            } else if parent == Self::branch_id() {
                2
            } else {
                0
            }
        }

        fn child_id(&self, parent: NodeId, index: usize) -> Option<NodeId> {
            self.child_id_queries
                .set(self.child_id_queries.get().saturating_add(1));
            if parent == 1 && index < HUGE_CHILDREN {
                Self::direct_id(index)
            } else if parent == Self::branch_id() {
                match index {
                    0 => Some(HUGE_GRANDCHILD_A),
                    1 => Some(HUGE_GRANDCHILD_B),
                    _ => None,
                }
            } else {
                None
            }
        }
    }

    struct DelegateProbe {
        label: String,
        height: f32,
        consume_press: bool,
        presses: Rc<Cell<usize>>,
        rect: Rect,
        cursor: Option<Point>,
    }

    impl DelegateProbe {
        fn new(
            label: impl Into<String>,
            height: f32,
            consume_press: bool,
            presses: Rc<Cell<usize>>,
        ) -> Self {
            Self {
                label: label.into(),
                height,
                consume_press,
                presses,
                rect: Rect::ZERO,
                cursor: None,
            }
        }
    }

    impl Widget for DelegateProbe {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let style = Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: Dimension::length(self.height),
                },
                ..Default::default()
            };
            taffy.widget_leaf(style, node_map, self)
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: taffy::NodeId, origin: Point) {
            self.rect = crate::widget::layout_rect(taffy, node, origin);
        }

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
            if let WindowEvent::CursorMoved { position, .. } = event {
                self.cursor = Some(Point::new(position.x, position.y));
            }
            if self.consume_press
                && self.cursor.is_some_and(|point| self.rect.contains(point))
                && matches!(
                    event,
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                        ..
                    }
                )
            {
                self.presses.set(self.presses.get().saturating_add(1));
                *ctx.handled = true;
            }
        }

        fn name(&self) -> &'static str {
            "DelegateProbe"
        }

        fn get_rect(&self) -> Rect {
            self.rect
        }

        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            Some(crate::a11y::AccessNode::new(crate::a11y::Role::Button).with_name(&self.label))
        }
    }

    pub(super) fn lay_out_tree<T: 'static, M: TreeModel + 'static>(
        view: &mut TreeView<T, M>,
        width: f32,
    ) -> (TaffyTree<()>, taffy::NodeId) {
        let doc = AutoCommit::new();
        view.sync(&doc, &automerge::ROOT);
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = view.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::Definite(f32_of_f64(view.body_height())),
                },
            )
            .unwrap();
        view.apply_layout(&taffy, node, Point::ZERO);
        (taffy, node)
    }

    #[test]
    fn unchanged_layout_preserves_tree_paint_while_theme_and_extent_invalidate() {
        fn layout(view: &mut TreeView<()>, theme: &Theme, width: f32) {
            let mut taffy = TaffyTree::new();
            let mut nodes = HashMap::new();
            let node = view.build_layout(&mut taffy, theme, &mut nodes);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(width),
                        height: taffy::AvailableSpace::Definite(240.0),
                    },
                )
                .unwrap();
            view.apply_layout(&taffy, node, Point::ZERO);
        }
        fn paint(view: &mut TreeView<()>, theme: &Theme, text: &mut TextRenderer) -> Scene {
            let mut scene = Scene::new();
            view.draw(
                &mut scene,
                Affine::IDENTITY,
                text,
                theme,
                &PresenceCtx::empty(),
            );
            scene
        }
        let dark = Theme::dark().with_reduce_motion(true);
        let light = Theme::light().with_reduce_motion(true);
        let mut view = TreeView::new(sample_tree()).hide_root();
        let mut text = TextRenderer::new();
        view.sync(&AutoCommit::new(), &automerge::ROOT);
        layout(&mut view, &dark, 280.0);
        let initial = paint(&mut view, &dark, &mut text);
        view.mark_drawn();
        layout(&mut view, &dark, 280.0);
        assert!(
            !view.needs_redraw(),
            "an identical layout must not dirty the sidebar"
        );
        let mut damage = Vec::new();
        view.collect_damage(&mut damage);
        assert!(damage.is_empty());
        assert!(initial.encoding_eq(&paint(&mut view, &dark, &mut text)));
        view.mark_drawn();
        layout(&mut view, &light, 280.0);
        assert!(
            view.needs_redraw(),
            "theme changes must invalidate retained ink"
        );
        let themed = paint(&mut view, &light, &mut text);
        assert!(!initial.encoding_eq(&themed));
        view.mark_drawn();
        layout(&mut view, &light, 360.0);
        assert!(
            view.needs_redraw(),
            "changed bounds must invalidate retained ink"
        );
        assert!(!themed.encoding_eq(&paint(&mut view, &light, &mut text)));
    }

    #[test]
    fn a_settled_tree_damages_the_row_that_changed_and_matches_gpu_pixels() {
        fn layout(view: &mut TreeView<()>, theme: &Theme) {
            let mut taffy = TaffyTree::new();
            let mut nodes = HashMap::new();
            let node = view.build_layout(&mut taffy, theme, &mut nodes);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(280.0),
                        height: taffy::AvailableSpace::Definite(240.0),
                    },
                )
                .unwrap();
            view.apply_layout(&taffy, node, Point::ZERO);
        }
        fn paint(view: &mut TreeView<()>, theme: &Theme, text: &mut TextRenderer) -> Scene {
            let mut scene = Scene::new();
            view.draw(
                &mut scene,
                Affine::IDENTITY,
                text,
                theme,
                &PresenceCtx::empty(),
            );
            scene
        }
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let theme = theme.with_reduce_motion(true);
            // Six pages, so one row is a sixth of the tree.
            let pages = (2..8)
                .map(|id| TreeNode::leaf(id, format!("page {id}"), ()))
                .collect();
            let mut view = TreeView::new(TreeNode::branch(1, "root", (), pages)).hide_root();
            let mut text = TextRenderer::new();
            view.sync(&AutoCommit::new(), &automerge::ROOT);
            layout(&mut view, &theme);
            let before = paint(&mut view, &theme, &mut text);
            view.mark_drawn();
            // The open item's row is set in the heavier weight.
            view.set_current(Some(3));
            layout(&mut view, &theme);
            let after = paint(&mut view, &theme, &mut text);
            let mut damage = Vec::new();
            view.collect_damage(&mut damage);
            view.mark_drawn();
            assert!(!damage.is_empty(), "the row changed");
            let area: f64 = damage.iter().map(Rect::area).sum();
            assert!(
                area < view.get_rect().area() / 4.0,
                "one row changed, not the tree: {damage:?} of {:?}",
                view.get_rect()
            );
            let background = theme.tokens.colors.surfaces.content;
            let mut retained = crate::offscreen::OffscreenRenderer::new(280, 240);
            let mut full = crate::offscreen::OffscreenRenderer::new(280, 240);
            retained.render(&before, background);
            let regions: Vec<_> = damage.iter().map(|rect| rect.inflate(1.0, 1.0)).collect();
            retained.render_damage_regions_gpu_wait(&after, background, &regions);
            assert_eq!(
                retained.readback_current(),
                full.render(&after, background),
                "the sparse repaint equals a full render"
            );
        }
    }

    pub(super) fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        crate::synthetic_input::cursor_moved(x, y)
    }

    pub(super) fn mouse_press() -> WindowEvent {
        crate::synthetic_input::mouse_press()
    }

    pub(super) fn mouse_release() -> WindowEvent {
        crate::synthetic_input::mouse_release()
    }

    pub(super) fn key_press(logical_key: Key) -> WindowEvent {
        crate::synthetic_input::keyboard_input(crate::synthetic_input::key_event(
            logical_key,
            None,
            ElementState::Pressed,
        ))
    }

    pub(super) fn dispatch_laid_out<T: 'static, M: TreeModel + 'static>(
        view: &mut TreeView<T, M>,
        event: &WindowEvent,
        taffy: &TaffyTree<()>,
        node: taffy::NodeId,
    ) -> bool {
        Dispatch::new()
            .layout(taffy, node)
            .send(view, event)
            .handled
    }

    #[test]
    fn expansion_toggles_from_disclosure_and_keys_and_persists_quietly() {
        // Case `disclosure_hit_zone_is_the_painted_column_at_two_x`.
        // At 2× text the disclosure column is painted at the `sm` line width
        // (30 px), not the 1× 16 px. A press between the two must toggle the
        // branch: the hit zone is the painted column, not the pinned constant.
        {
            let root = TreeNode::branch(1, "crates", (), vec![TreeNode::leaf(2, "main.rs", ())]);
            let mut view = TreeView::new(root);
            let doc = AutoCommit::new();
            view.sync(&doc, &automerge::ROOT);
            let mut taffy = TaffyTree::new();
            let mut node_map = HashMap::new();
            let theme = Theme::dark().with_text_scale(2.0);
            let node = view.build_layout(&mut taffy, &theme, &mut node_map);
            taffy
                .compute_layout(
                    node,
                    Size {
                        width: taffy::AvailableSpace::Definite(320.0),
                        height: taffy::AvailableSpace::Definite(f32_of_f64(view.body_height())),
                    },
                )
                .unwrap();
            view.apply_layout(&taffy, node, Point::ZERO);
            assert!(
                view.disclosure_w > DISCLOSURE_WIDTH,
                "2x resolves a wider disclosure column: {}",
                view.disclosure_w
            );
            assert!(view.viewport.estimated_height() > ROW_HEIGHT);
            assert!(!view.expanded.contains(&1));

            let x = f64::midpoint(DISCLOSURE_WIDTH, view.disclosure_w);
            let y = view.viewport.estimated_height() / 2.0;
            dispatch_laid_out(&mut view, &cursor_moved(x, y), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
            assert!(
                view.expanded.contains(&1),
                "a press at x={x} inside the painted disclosure column toggles the branch"
            );
        }

        // Case `tree_view_expand_node_shows_children`.
        {
            let mut view = TreeView::new(sample_tree());
            let rows = view.visible_rows();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].0, 1);

            view.toggle_expand(1);
            let rows = view.visible_rows();
            let ids: Vec<NodeId> = rows.iter().map(|(id, _, _)| *id).collect();
            assert!(
                ids.contains(&2),
                "child1 must be visible after expanding root"
            );
            assert!(
                ids.contains(&3),
                "child2 must be visible after expanding root"
            );
            assert_eq!(rows.len(), 3);
        }

        // Case `tree_view_arrow_right_expands_focused_node`.
        {
            let mut view = TreeView::new(sample_tree());
            view.set_focus(true);
            view.focused = Some(TreeRowKey::Node(1));
            assert!(!view.is_expanded(1));

            dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowRight)));

            assert!(
                view.is_expanded(1),
                "ArrowRight must expand the focused branch"
            );
        }

        // Case `empty_branch_remains_expandable_for_lazy_population`.
        {
            let mut view = TreeView::new(TreeNode::branch(1, "loading", (), Vec::new()));
            view.toggle_expand(1);
            assert!(view.is_expanded(1));
            let node = Widget::accessibility(&view).unwrap();
            assert_eq!(node.children[0].state.expanded, Some(true));
        }

        // Case `expansion_has_a_persistable_builder_and_change_callback`.
        {
            let changes = Rc::new(RefCell::new(Vec::new()));
            let sink = changes.clone();
            let mut view = TreeView::new(sample_tree())
                .with_expanded([1])
                .with_accessible_name("Project files")
                .on_expand(move |id, expanded| sink.borrow_mut().push((id, expanded)));

            assert!(view.is_expanded(1));
            assert!(view.set_expanded(1, false));
            assert!(!view.set_expanded(1, false), "idempotent controlled update");
            assert!(!view.set_expanded(2, true), "a leaf cannot expand");
            assert_eq!(*changes.borrow(), vec![(1, false)]);
            assert_eq!(
                Widget::accessibility(&view).unwrap().name.as_deref(),
                Some("Project files")
            );
        }

        // Case `restoring_expansion_does_not_echo_to_the_host`.
        {
            let echoed = Rc::new(Cell::new(0));
            let count = Rc::clone(&echoed);
            let mut view = TreeView::new(navigator_tree())
                .hide_root()
                .on_expand(move |_, _| count.set(count.get() + 1));
            view.restore_expanded([2, 5, 99]);
            assert!(view.is_expanded(2));
            assert!(!view.is_expanded(5), "leaves are not expandable");
            assert_eq!(echoed.get(), 0);
            view.set_expanded(2, false);
            assert_eq!(echoed.get(), 1, "a user change still reports");
        }

        // Case `the_disclosure_toggles_without_activating`.
        {
            let (view, activated, _) = logged(
                TreeView::new(navigator_tree())
                    .hide_root()
                    .click_activation(TreeClickActivation::SingleClick),
            );
            let mut view = view;
            let (taffy, node) = lay_out_tree(&mut view, 240.0);
            // The caret column starts at the list's s2 inset, never on the
            // pane edge.
            let caret = view.disclosure_rect(2).expect("caret");
            assert!(caret.x0 >= view.rect.x0 + f64::from(Theme::dark().tokens.spacing.s2));
            let at = Point::new(caret.x0 + 2.0, row_center(&view, 2).y);
            dispatch_laid_out(&mut view, &cursor_moved(at.x, at.y), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
            assert!(view.is_expanded(2));
            assert!(activated.borrow().is_empty());
        }
    }

    #[test]
    fn selection_reports_callbacks_and_multiselect_by_stable_id() {
        // Case `tree_view_select_node_calls_on_select`.
        {
            let captured: Rc<RefCell<Option<NodeId>>> = Rc::new(RefCell::new(None));
            let captured_clone = captured.clone();
            let mut view = TreeView::new(sample_tree()).on_select(move |id| {
                *captured_clone.borrow_mut() = Some(id);
            });
            view.toggle_expand(1);

            // Taffy's `auto` width on a leaf inside a block container collapses
            // to zero under `MAX_CONTENT`, so wrap it in a fixed-width parent
            // and patch `view.rect` manually rather than trust
            // `apply_layout`'s width.
            let mut taffy = TaffyTree::<()>::new();
            let mut node_map: HashMap<taffy::NodeId, *mut dyn Widget> = HashMap::new();
            let node = view.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
            let root_taffy = taffy.parent_node(
                Style {
                    size: Size::from_lengths(200.0, 80.0),
                    ..Default::default()
                },
                &[node],
            );
            taffy
                .compute_layout(root_taffy, taffy::Size::MAX_CONTENT)
                .unwrap();
            view.apply_layout(&taffy, node, Point::ZERO);
            view.rect = Rect::new(
                0.0,
                0.0,
                200.0,
                f64_of_usize(view.visible_rows().len()) * ROW_HEIGHT,
            );
            let target_id = view.visible_rows()[1].0;
            let target_rect = Rect::new(0.0, ROW_HEIGHT, 200.0, ROW_HEIGHT * 2.0);
            assert_eq!(target_id, 2);
            let click_x = target_rect.x0 + 60.0; // past depth-1 indent + glyph
            let click_y = target_rect.y0 + target_rect.height() / 2.0;

            dispatch(&mut view, &cursor_moved(click_x, click_y));
            dispatch(&mut view, &mouse_press());
            assert_eq!(
                *captured.borrow(),
                None,
                "selection waits until the gesture is known to be a click"
            );
            dispatch(&mut view, &mouse_release());

            assert_eq!(
                *captured.borrow(),
                Some(2),
                "on_select must fire with child1 id"
            );
            assert_eq!(view.selected(), Some(2));
        }

        // Case `multiselect_uses_stable_ids_and_survives_collapse_and_move`.
        {
            let mut view = TreeView::from_model(external_model())
                .with_expanded([1, 2])
                .multi_select(true);
            view.apply_selection_gesture(20, false, false);
            view.apply_selection_gesture(3, true, false);
            assert!(view.selection().is_selected(&20));
            assert!(view.selection().is_selected(&3));

            view.set_expanded(2, false);
            assert!(
                view.selection().is_selected(&20),
                "hidden nodes stay selected"
            );
            view.update_source(
                &[TreeModelChange::RowsMoved {
                    parent: 1,
                    from: 1,
                    to: 0,
                    count: 1,
                }],
                |model| {
                    let children = &mut model.nodes.get_mut(&1).unwrap().children;
                    let moved = children.remove(1);
                    children.insert(0, moved);
                },
            );
            assert!(view.selection().is_selected(&3));
            assert!(view.selection().is_selected(&20));
        }

        // Case `multiselect_supports_visible_ranges_and_constant_space_select_all`.
        {
            let mut view = TreeView::from_model(external_model())
                .with_expanded([1, 2])
                .multi_select(true);
            view.apply_selection_gesture(2, false, false);
            view.apply_selection_gesture(4, false, true);
            assert!(view.selection().is_selected(&2));
            assert!(view.selection().is_selected(&20));
            assert!(view.selection().is_selected(&3));
            assert!(view.selection().is_selected(&4));

            assert!(view.on_key_with_modifiers(&Key::Character("a".into()), true, false));
            assert!(view.selection().is_select_all());
            assert_eq!(view.selection().stored_keys().count(), 0);
            assert!(view.selection().is_selected(&20));

            view.apply_selection_gesture(3, true, false);
            assert!(!view.selection().is_selected(&3));
            assert_eq!(
                view.selection().stored_keys().copied().collect::<Vec<_>>(),
                vec![3]
            );
        }
    }

    // USER REQUIREMENT (2026-10-19): "Typing in a focused list jumps to
    // matches." Letters typed into a focused list move to the first row whose
    // name begins with them; the same letter again steps to the next one.
    #[test]
    fn typing_in_a_focused_list_jumps_to_the_row_it_names() {
        let names = ["Apple", "apricot", "Banana", "berry", "Cherry"];
        let children = names
            .iter()
            .enumerate()
            .map(|(index, name)| TreeNode::leaf(index as u64 + 2, (*name).to_owned(), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children))
            .hide_root()
            .selection_follows_focus(true);
        view.set_focus(true);
        view.focused = Some(TreeRowKey::Node(2));

        dispatch(&mut view, &key_press(Key::Character("c".into())));
        assert_eq!(view.focused(), Some(6), "c moves to Cherry");
        assert_eq!(view.selected(), Some(6), "a navigation list selects it too");

        dispatch(&mut view, &key_press(Key::Character("b".into())));
        // Within the pause the letters spell "cb", which names nothing; the
        // list stays where it is rather than jumping somewhere unexpected.
        assert_eq!(view.focused(), Some(6));
    }

    #[test]
    fn keyboard_navigation_moves_focus_and_separates_selection_from_activation() {
        // Case `tree_view_arrow_down_moves_focus_to_next_visible_row`.
        {
            let mut view = TreeView::new(sample_tree());
            view.toggle_expand(1);
            view.set_focus(true);
            view.focused = Some(TreeRowKey::Node(1));

            dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowDown)));

            let rows = view.visible_rows();
            let second_id = rows[1].0;
            assert_eq!(
                view.focused(),
                Some(second_id),
                "ArrowDown must move focus from row 0 to row 1",
            );
        }

        // Case `keyboard_navigation_reveals_focus_and_follows_tree_parents`.
        {
            let children = (2_u64..=20)
                .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
                .collect();
            let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
            view.toggle_expand(1);
            view.set_viewport(44.0, f32_of_f64(ROW_HEIGHT));
            view.set_focus(true);
            view.focused = Some(TreeRowKey::Node(1));

            dispatch(&mut view, &key_press(Key::Named(NamedKey::End)));
            assert_eq!(view.focused(), Some(20));
            assert!(view.scroll_y() > 0.0, "End reveals the final row");

            dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowLeft)));
            assert_eq!(view.focused(), Some(1), "Left on a leaf focuses its parent");
            assert_eq!(view.scroll_y(), 0.0, "revealing the root returns to top");
        }

        // Case `enter_activates_while_space_and_arrows_only_select`.
        {
            let (view, activated, selected) = logged(
                TreeView::new(navigator_tree())
                    .hide_root()
                    .selection_follows_focus(true),
            );
            let mut view = view;
            view.set_focus(true);
            view.focused = Some(TreeRowKey::Node(2));

            dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowDown)));
            assert_eq!(view.focused(), Some(5));
            assert_eq!(view.selected(), Some(5), "selection follows keyboard focus");
            assert!(activated.borrow().is_empty(), "browsing opens nothing");

            dispatch(&mut view, &key_press(Key::Character(" ".into())));
            assert!(
                activated.borrow().is_empty(),
                "Space selects without activating"
            );

            dispatch(&mut view, &key_press(Key::Named(NamedKey::Enter)));
            assert_eq!(*activated.borrow(), [5], "Enter activates the focused row");
            assert!(selected.borrow().contains(&5));
        }

        // Case `arrows_do_not_select_unless_selection_follows_focus`.
        {
            let (mut view, activated, _) = logged(TreeView::new(navigator_tree()).hide_root());
            view.set_focus(true);
            view.focused = Some(TreeRowKey::Node(2));
            dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowDown)));
            assert_eq!(view.focused(), Some(5));
            assert_eq!(view.selected(), None);
            assert!(activated.borrow().is_empty());
        }

        // Case `an_unfocused_tree_leaves_keys_to_its_siblings`.
        {
            let (mut view, activated, _) = logged(TreeView::new(navigator_tree()).hide_root());
            view.focused = Some(TreeRowKey::Node(2));
            dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowDown)));
            dispatch(&mut view, &key_press(Key::Named(NamedKey::Enter)));
            assert_eq!(view.focused(), Some(2), "keys reach only a focused tree");
            assert!(activated.borrow().is_empty());
        }
    }

    #[test]
    fn accessibility_reports_tree_role_value_focus_and_row_state() {
        // Case `accessibility_reports_tree_role_and_no_value_when_unselected`.
        {
            use crate::a11y::Role;
            let view = TreeView::new(sample_tree());
            let node = Widget::accessibility(&view).expect("tree is not presentational");
            assert_eq!(node.role, Role::Tree);
            assert_eq!(node.name, None, "the tree has no intrinsic label");
            assert_eq!(node.value, None, "no value until a row is selected");
            assert!(node.state.focusable);
            assert!(!node.state.disabled);
            assert!(!node.state.focused);
        }

        // Case `tree_view_exposes_an_explicit_stable_id`.
        {
            let view = TreeView::new(sample_tree()).with_id("workspace-tree");
            assert_eq!(Widget::test_id(&view), Some("workspace-tree"));
        }

        // Case `accessibility_value_is_selected_row_label`.
        {
            let mut view = TreeView::new(sample_tree());
            view.toggle_expand(1);
            view.select(2); // "child1"
            let node = Widget::accessibility(&view).unwrap();
            assert_eq!(
                node.value.as_deref(),
                Some("child1"),
                "value is the selected node's label",
            );
        }

        // Case `accessibility_reflects_focus_state`.
        {
            let mut view = TreeView::new(sample_tree());
            view.set_focus(true);
            let node = Widget::accessibility(&view).unwrap();
            assert!(
                node.state.focused,
                "widget focus surfaces in the access node"
            );
        }

        // Case `tree_items_publish_level_expansion_selection_and_focus`.
        {
            let mut view = TreeView::new(sample_tree());
            view.toggle_expand(1);
            view.select(2);
            view.focused = Some(TreeRowKey::Node(2));
            view.set_focus(true);
            let node = Widget::accessibility(&view).unwrap();

            assert_eq!(node.children.len(), 3);
            assert_eq!(node.children[0].state.expanded, Some(true));
            assert_eq!(node.children[0].level, Some(1));
            assert_eq!(node.children[1].level, Some(2));
            assert!(node.children[1].state.selected);
            assert!(node.children[1].state.focused);
            assert_eq!(node.children[1].name.as_deref(), Some("child1"));
        }

        // Case `decorations_see_row_state_and_describe_rows`.
        {
            let seen = Rc::new(RefCell::new(Vec::new()));
            let log = Rc::clone(&seen);
            let mut view = TreeView::new(navigator_tree())
                .hide_root()
                .with_row_decoration(move |_, row, theme| {
                    log.borrow_mut().push(row);
                    TreeRowDecoration::new()
                        .icon(
                            if row.is_branch {
                                Icon::Folder
                            } else {
                                Icon::File
                            },
                            theme.tokens.colors.text_dim,
                        )
                        .trailing("3")
                        .description(if row.is_branch { "Folder" } else { "File" })
                });
            view.set_current(Some(5));
            view.select(2);
            let _ = lay_out_tree(&mut view, 240.0);
            draw_view(&mut view);
            let rows = seen.borrow().clone();
            let readme = rows
                .iter()
                .find(|row| row.id == 5)
                .expect("README decorated");
            assert!(readme.current && !readme.selected && !readme.is_branch);
            let src = rows.iter().find(|row| row.id == 2).expect("src decorated");
            assert!(src.selected && src.is_branch && !src.expanded && !src.current);

            let semantics = Widget::accessibility(&view).unwrap();
            let described: Vec<_> = semantics
                .children
                .iter()
                .map(|item| item.description.clone())
                .collect();
            assert_eq!(
                described,
                [Some("Folder".to_owned()), Some("File".to_owned())]
            );
            assert_eq!(view.current(), Some(5));
            view.set_current(Some(99));
            assert_eq!(view.current(), None, "an unknown id is not current");
        }
    }

    #[test]
    fn bounded_viewport_windows_paint_and_semantics_around_a_stable_anchor() {
        // Case `tree_viewport_builder_enables_the_bounded_internal_window`.
        {
            let children = (2_u64..102)
                .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
                .collect();
            let view = TreeView::new(TreeNode::branch(1, "root", (), children))
                .with_expanded([1])
                .with_viewport(44.0, 30.0);
            assert_eq!(view.viewport.viewport(), 44.0);
            assert_eq!(view.viewport.estimated_height(), 30.0);
            assert!(view.visible_row_range().len() < view.visible_len());
        }

        // Case `large_tree_bounds_paint_and_accessibility_windows`.
        {
            let children = (2_u64..=100_001)
                .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
                .collect();
            let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
            view.toggle_expand(1);
            view.set_viewport(110.0, f32_of_f64(ROW_HEIGHT));
            view.set_scroll_y(view.max_scroll_y() / 2.0);

            let visible = view.visible_row_range();
            assert!(visible.len() <= 12, "bounded geometry window: {visible:?}");
            let node = Widget::accessibility(&view).unwrap();
            assert!(node.children.len() <= 12, "bounded semantic window");
            let first = node.children.first().expect("visible tree item");
            assert_eq!(first.role, crate::a11y::Role::TreeItem);
            assert_eq!(first.set_size, Some(100_001));
            assert!(first.pos_in_set.expect("absolute position") > 49_000);
        }

        // Case `semantic_tree_rows_publish_scrolled_viewport_geometry`.
        {
            let mut view = TreeView::new(sample_tree());
            view.toggle_expand(1);
            view.set_viewport(f32_of_f64(ROW_HEIGHT * 2.0), f32_of_f64(ROW_HEIGHT));
            view.set_scroll_y(10.0);
            let _ = lay_out_tree(&mut view, 320.0);

            let semantics = Widget::accessibility(&view).expect("tree semantics");
            let first = semantics.children.first().expect("first visible row");
            assert_eq!(
                first.bounds.expect("tree row bounds").to_rect(),
                Rect::new(0.0, -10.0, 320.0, ROW_HEIGHT - 10.0),
            );
        }

        // Case `expanding_above_viewport_preserves_the_stable_row_anchor`.
        {
            let root = TreeNode::branch(
                1,
                "root",
                (),
                vec![
                    TreeNode::branch(
                        2,
                        "folder",
                        (),
                        vec![
                            TreeNode::leaf(3, "a", ()),
                            TreeNode::leaf(4, "b", ()),
                            TreeNode::leaf(5, "c", ()),
                        ],
                    ),
                    TreeNode::leaf(10, "anchor", ()),
                    TreeNode::leaf(11, "after", ()),
                ],
            );
            let mut view = TreeView::new(root);
            view.toggle_expand(1);
            view.set_viewport(f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
            view.set_scroll_y(f32_of_f64(ROW_HEIGHT * 2.0 + 5.0));
            let before = view.viewport.item_view_top(2);

            view.toggle_expand(2);

            let anchor = view.visible_index_of(10).expect("anchor remains visible");
            assert_eq!(view.viewport.item_view_top(anchor), before);
        }
    }

    #[test]
    fn external_model_changes_reconcile_state_and_keep_anchors() {
        // Case `batched_model_updates_reconcile_lazy_children_and_deleted_selection`.
        {
            let mut view =
                TreeView::new(TreeNode::branch(1, "root", (), Vec::new())).with_expanded([1]);
            view.update_model(|root| {
                root.children.push(TreeNode::leaf(2, "loaded", ()));
            });
            assert_eq!(
                view.visible_rows()
                    .iter()
                    .map(|(id, _, _)| *id)
                    .collect::<Vec<_>>(),
                vec![1, 2]
            );

            view.select(2);
            view.focused = Some(TreeRowKey::Node(2));
            view.update_model(|root| root.children.clear());
            assert_eq!(view.selected(), None, "deleted ids leave selection sane");
            assert_eq!(view.focused(), None, "deleted ids leave focus sane");
            assert!(view.is_expanded(1), "the surviving lazy branch stays open");
        }

        // Case `external_model_queries_only_expanded_branches_and_preserves_insert_anchor`.
        {
            let mut view = TreeView::from_model(external_model());
            assert_eq!(
                view.model().child_queries.get(),
                0,
                "a collapsed root does not query its children"
            );
            view = view.with_expanded([1]);
            assert_eq!(view.model().child_queries.get(), 1);
            assert_eq!(
                view.visible_rows()
                    .iter()
                    .map(|(id, _, _)| *id)
                    .collect::<Vec<_>>(),
                vec![1, 2, 3, 4],
                "the collapsed folder's child is not queried into visible order"
            );

            view.set_viewport(f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
            view.set_scroll_y(f32_of_f64(ROW_HEIGHT * 2.0 + 5.0));
            let before = view
                .viewport
                .item_view_top(view.visible_index_of(3).unwrap());
            view.update_source(
                &[TreeModelChange::RowsInserted {
                    parent: 1,
                    index: 0,
                    count: 1,
                }],
                |model| {
                    model.nodes.insert(
                        5,
                        ExternalNode {
                            label: "inserted".into(),
                            branch: false,
                            children: Vec::new(),
                        },
                    );
                    model.nodes.get_mut(&1).unwrap().children.insert(0, 5);
                },
            );
            let anchor = view.visible_index_of(3).expect("stable id remains");
            assert_eq!(view.viewport.item_view_top(anchor), before);
        }

        // Case `post_mutation_notification_uses_the_cached_stable_anchor`.
        {
            let children = Rc::new(RefCell::new(vec![2, 3, 4]));
            let mut view = TreeView::from_model(SharedOrderModel {
                children: Rc::clone(&children),
            })
            .with_expanded([1]);
            view.set_viewport(f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
            view.set_scroll_offset(ROW_HEIGHT * 2.0 + 5.0);
            let before = view
                .viewport
                .item_view_top(view.visible_index_of(3).unwrap());

            children.borrow_mut().insert(0, 5);
            view.notify_model_changes(&[TreeModelChange::RowsInserted {
                parent: 1,
                index: 0,
                count: 1,
            }]);

            let anchor = view.visible_index_of(3).expect("stable id remains");
            assert_eq!(view.viewport.item_view_top(anchor), before);
        }

        // Case `granular_external_changes_refresh_data_and_reconcile_removed_state`.
        {
            let mut view = TreeView::from_model(external_model())
                .with_expanded([1])
                .with_selected(Some(3));
            view.focused = Some(TreeRowKey::Node(3));
            view.set_viewport(66.0, f32_of_f64(ROW_HEIGHT));
            let offset = view.scroll_y();

            view.update_source(&[TreeModelChange::RowsChanged { ids: vec![3] }], |model| {
                model.nodes.get_mut(&3).unwrap().label = "renamed".into();
            });
            assert_eq!(view.scroll_y(), offset, "data changes preserve geometry");
            assert!(
                Widget::accessibility(&view)
                    .unwrap()
                    .children
                    .iter()
                    .any(|row| row.name.as_deref() == Some("renamed")),
                "semantics query the fresh model label"
            );

            view.update_source(
                &[TreeModelChange::RowsRemoved {
                    parent: 1,
                    index: 1,
                    count: 1,
                }],
                |model| {
                    model.nodes.get_mut(&1).unwrap().children.remove(1);
                    model.nodes.remove(&3);
                },
            );
            assert_eq!(view.selected(), None);
            assert_eq!(view.focused(), None);
            assert_eq!(view.visible_index_of(3), None);
        }

        // Case `external_move_keeps_measurements_attached_to_node_ids`.
        {
            let mut view = TreeView::from_model(external_model()).with_expanded([1]);
            let old_index = view.visible_index_of(3).unwrap();
            assert!(view.set_row_height(old_index, 73.0));

            view.update_source(
                &[TreeModelChange::RowsMoved {
                    parent: 1,
                    from: 1,
                    to: 0,
                    count: 1,
                }],
                |model| {
                    let children = &mut model.nodes.get_mut(&1).unwrap().children;
                    let moved = children.remove(1);
                    children.insert(0, moved);
                },
            );

            let new_index = view.visible_index_of(3).unwrap();
            assert_eq!(new_index, 1);
            assert_eq!(view.viewport.item_height(new_index), Some(73.0));
        }
    }

    #[test]
    fn scroll_input_pages_wheels_and_drags_over_sparse_rows() {
        // Case `page_navigation_and_accelerated_wheel_share_sparse_row_geometry`.
        {
            let children = (2_u64..=101)
                .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
                .collect();
            let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
            view.toggle_expand(1);
            view.set_viewport(100.0, f32_of_f64(ROW_HEIGHT));
            assert!(view.set_row_height(0, 300.0));
            assert!(view.focus_at(0));

            assert!(view.on_key_with_modifiers(&Key::Named(NamedKey::PageDown), false, false,));
            assert_eq!(view.focused(), Some(1));
            assert_eq!(view.scroll_y(), 90.0);
            assert!(view.on_key_with_modifiers(&Key::Named(NamedKey::PageUp), false, false,));
            assert_eq!(view.scroll_y(), 0.0);

            assert!(view.on_wheel(&MouseScrollDelta::LineDelta(0.0, -1.0), TouchPhase::Moved));
            let immediate = view.scroll_y();
            assert_eq!(immediate, 60.0);
            assert!(view.needs_animation_frame());
            let frame = crate::time::now() + Duration::from_millis(16);
            // The glide moves only the view: it is no change of the model.
            assert!(!crate::time::testing::with_monotonic_time(frame, || {
                Widget::update(&mut view, &AutoCommit::new(), &automerge::ROOT)
            }));
            assert!(view.scroll_y() > immediate, "momentum advances after input");
        }

        // Case `body_drag_scrolls_tree_without_selecting_but_click_still_selects`.
        {
            let children = (2_u64..=101)
                .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
                .collect();
            let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
            view.toggle_expand(1);
            view.set_viewport(100.0, f32_of_f64(ROW_HEIGHT));
            let (taffy, node) = lay_out_tree(&mut view, 320.0);

            dispatch_laid_out(&mut view, &cursor_moved(100.0, 80.0), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
            assert!(view.wants_pointer_capture());
            dispatch_laid_out(&mut view, &cursor_moved(100.0, 20.0), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
            assert_eq!(view.scroll_y(), 60.0);
            assert_eq!(view.selected(), None, "panning does not select a row");

            view.set_scroll_y(0.0);
            dispatch_laid_out(&mut view, &cursor_moved(100.0, 10.0), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
            assert_eq!(
                view.selected(),
                Some(1),
                "release below threshold is a click"
            );
        }

        // Case `reduced_motion_keeps_direct_scroll_without_wheel_or_drag_decay`.
        {
            let children = (2_u64..102)
                .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
                .collect();
            let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
            view.set_expanded(1, true);
            view.set_viewport(100.0, 22.0);
            view.reduce_motion = true;

            assert!(view.on_wheel(&MouseScrollDelta::LineDelta(0.0, -1.0), TouchPhase::Moved));
            assert_eq!(view.scroll_y(), 60.0_f32);
            assert!(!view.momentum.is_active());

            let now = crate::time::now();
            let started = now.checked_sub(Duration::from_millis(20)).unwrap_or(now);
            let moved = now.checked_sub(Duration::from_millis(5)).unwrap_or(now);
            view.drag_scroll
                .begin(50.0, f64::from(view.scroll_y()), started);
            assert!(view.drag_scroll.update(10.0, moved).is_some());
            dispatch(&mut view, &mouse_release());
            assert!(!view.momentum.is_active());
            assert!(!view.needs_animation_frame());
        }
    }

    #[test]
    fn paged_tree_demands_visible_slots_and_recovers_from_errors() {
        // Case `paged_tree_demands_only_the_visible_window_once`.
        {
            let requests = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&requests);
            let mut view = TreeView::from_model(PagedModel {
                count: 100_000_000,
                states: Rc::new(RefCell::new(HashMap::new())),
                self_loading: false,
                requests: Vec::new(),
                changes: Vec::new(),
            })
            .with_expanded([1])
            .on_load_request(move |request| captured.borrow_mut().push(request));
            view.set_viewport(66.0, f32_of_f64(ROW_HEIGHT));
            view.set_scroll_offset(f64_of_usize(50_000_000) * ROW_HEIGHT);

            let demanded = view.request_visible_rows();
            assert!(demanded > 0);
            assert!(demanded <= 8, "overscan must keep page demand bounded");
            assert_eq!(view.request_visible_rows(), 0, "requests are deduplicated");
            let requests = requests.borrow();
            assert_eq!(requests.len(), demanded);
            assert!(
                requests
                    .iter()
                    .all(|request| request.reason == TreeLoadReason::Visible)
            );
            assert!(requests.iter().all(|request| {
                request.slot.index.abs_diff(50_000_000) <= 5 && request.slot.parent == 1
            }));
            assert_eq!(view.visible_len(), 100_000_001);
        }

        // Case `model_owned_paged_tree_drains_slot_completions_during_update`.
        {
            let mut view = TreeView::from_model(PagedModel {
                count: 4,
                states: Rc::new(RefCell::new(HashMap::new())),
                self_loading: true,
                requests: Vec::new(),
                changes: Vec::new(),
            })
            .with_expanded([1]);
            view.set_viewport(66.0, f32_of_f64(ROW_HEIGHT));
            assert!(matches!(
                view.index.row_at(&view.model, 1).map(|(key, _)| key),
                Some(TreeRowKey::Slot(_))
            ));
            assert!(view.focus_at(1));

            let demanded = view.request_visible_rows();
            assert!(demanded > 0);
            assert_eq!(view.model().requests.len(), demanded);

            let document = AutoCommit::new();
            assert!(Widget::update(&mut view, &document, &automerge::ROOT));

            let first = PagedModel::id(0).expect("stable paged id");
            assert_eq!(
                view.index.row_at(&view.model, 1).map(|(key, _)| key),
                Some(TreeRowKey::Node(first))
            );
            assert_eq!(view.focused(), Some(first));
            assert_eq!(view.visible_row(1), Some((first, "loaded row", 1)));
            assert_eq!(view.request_visible_rows(), 0);
        }

        // Case `paged_error_is_keyboard_retryable_and_ready_row_inherits_focus_and_anchor`.
        {
            let states = Rc::new(RefCell::new(HashMap::from([(0, PagedSlotState::Error)])));
            let requests = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&requests);
            let mut view = TreeView::from_model(PagedModel {
                count: 1,
                states: Rc::clone(&states),
                self_loading: false,
                requests: Vec::new(),
                changes: Vec::new(),
            })
            .with_expanded([1])
            .on_load_request(move |request| captured.borrow_mut().push(request));
            view.set_viewport(f32_of_f64(ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
            view.set_scroll_offset(ROW_HEIGHT + 5.0);
            assert!(view.focus_at(1));
            view.set_focus(true);
            let before = view.viewport.item_view_top(1);

            let semantics = Widget::accessibility(&view).unwrap();
            let error = semantics
                .children
                .iter()
                .find(|row| row.state.invalid)
                .expect("error placeholder is exposed");
            assert!(error.state.focusable);
            assert!(error.state.focused);
            assert!(error.name.as_deref().unwrap().contains("Activate to retry"));

            assert!(view.on_key_with_modifiers(&Key::Named(NamedKey::Enter), false, false));
            assert_eq!(
                requests.borrow().as_slice(),
                &[TreeLoadRequest {
                    slot: TreeSlot {
                        parent: 1,
                        index: 0,
                    },
                    reason: TreeLoadReason::Retry,
                }]
            );

            states.borrow_mut().insert(0, PagedSlotState::Ready);
            view.notify_model_changes(&[TreeModelChange::SlotsChanged {
                slots: vec![TreeSlot {
                    parent: 1,
                    index: 0,
                }],
            }]);

            let id = PagedModel::id(0).unwrap();
            assert_eq!(view.focused(), Some(id));
            assert_eq!(view.visible_row(1), Some((id, "loaded row", 1)));
            assert_eq!(view.viewport.item_view_top(1), before);
            let loaded = Widget::accessibility(&view)
                .unwrap()
                .children
                .into_iter()
                .find(|row| row.name.as_deref() == Some("loaded row"))
                .expect("loaded row replaces placeholder semantics");
            assert!(!loaded.state.busy);
            assert!(!loaded.state.invalid);
            assert!(loaded.state.focused);
        }

        // Case `paged_slot_delegate_localizes_measures_and_rebuilds_changed_placeholders`.
        {
            let states = Rc::new(RefCell::new(HashMap::from([
                (0, PagedSlotState::Loading),
                (1, PagedSlotState::Error),
            ])));
            let builds = Rc::new(Cell::new(0_usize));
            let captured = Rc::clone(&builds);
            let mut view = TreeView::from_model(PagedModel {
                count: 2,
                states: Rc::clone(&states),
                self_loading: false,
                requests: Vec::new(),
                changes: Vec::new(),
            })
            .with_expanded([1])
            .with_slot_delegate(move |_, slot, _, state| {
                captured.set(captured.get().saturating_add(1));
                let (label, height) = match state {
                    TreeSlotState::Loading => (format!("Chargement {}", slot.index), 34.0),
                    TreeSlotState::Error { message } => (
                        format!("Réessayer {} — {}", slot.index, message.unwrap_or("Erreur")),
                        48.0,
                    ),
                };
                Box::new(DelegateProbe::new(
                    label,
                    height,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            });
            view.set_viewport(120.0, f32_of_f64(ROW_HEIGHT));
            let _ = lay_out_tree(&mut view, 360.0);

            assert_eq!(builds.get(), 2);
            assert_eq!(view.viewport.item_height(1), Some(34.0));
            assert_eq!(view.viewport.item_height(2), Some(48.0));
            let semantics = Widget::accessibility(&view).unwrap();
            let loading = semantics
                .children
                .iter()
                .find(|row| row.action_target.as_deref() == Some("slot:1:0"))
                .expect("localized loading slot semantics");
            assert_eq!(loading.name.as_deref(), Some("Chargement 0"));
            assert!(loading.state.busy);
            assert_eq!(
                loading
                    .children
                    .first()
                    .expect("delegated child semantics")
                    .role,
                crate::a11y::Role::Button
            );
            let error = semantics
                .children
                .iter()
                .find(|row| row.action_target.as_deref() == Some("slot:1:1"))
                .expect("localized error slot semantics");
            assert_eq!(
                error.name.as_deref(),
                Some("Réessayer 1 — Network unavailable")
            );
            assert!(error.state.invalid);

            states.borrow_mut().insert(0, PagedSlotState::Error);
            view.notify_model_changes(&[TreeModelChange::SlotsChanged {
                slots: vec![TreeSlot {
                    parent: 1,
                    index: 0,
                }],
            }]);
            let _ = lay_out_tree(&mut view, 360.0);
            assert_eq!(builds.get(), 3, "only the changed slot is rebuilt");
            let changed = Widget::accessibility(&view)
                .unwrap()
                .children
                .into_iter()
                .find(|row| row.action_target.as_deref() == Some("slot:1:0"))
                .expect("changed slot semantics");
            assert_eq!(
                changed.name.as_deref(),
                Some("Réessayer 0 — Network unavailable")
            );
            assert!(changed.state.invalid);
        }
    }

    #[test]
    fn hundred_million_child_trees_stay_sparse_end_to_end() {
        // Case `sparse_tree_index_resolves_a_hundred_million_children`.
        {
            let model = HugeModel {
                child_id_queries: Cell::new(0),
            };
            let branch = HugeModel::branch_id();
            let expanded = HashSet::from([1, branch]);
            let index = SparseTreeIndex::build(&model, &expanded, false);

            assert_eq!(index.len(), 100_000_003);
            assert_eq!(index.row_at(&model, 0), Some((TreeRowKey::Node(1), 0)));
            assert_eq!(
                index.row_at(&model, HUGE_BRANCH_POSITION.saturating_add(1)),
                Some((TreeRowKey::Node(branch), 1))
            );
            assert_eq!(
                index.row_at(&model, HUGE_BRANCH_POSITION.saturating_add(2)),
                Some((TreeRowKey::Node(HUGE_GRANDCHILD_A), 2))
            );
            assert_eq!(
                index.row_at(&model, HUGE_BRANCH_POSITION.saturating_add(3)),
                Some((TreeRowKey::Node(HUGE_GRANDCHILD_B), 2))
            );
            let after_branch =
                HugeModel::direct_id(HUGE_BRANCH_POSITION.saturating_add(1)).unwrap();
            assert_eq!(
                index.row_at(&model, HUGE_BRANCH_POSITION.saturating_add(4)),
                Some((TreeRowKey::Node(after_branch), 1))
            );
            assert_eq!(index.index_of(&model, HUGE_GRANDCHILD_B), Some(50_000_003));
            assert_eq!(index.index_of(&model, after_branch), Some(50_000_004));
            assert!(
                model.child_id_queries.get() < 10,
                "resolution must not scan the enormous sibling set"
            );
        }

        // Case `tree_view_virtualizes_a_hundred_million_child_model_end_to_end`.
        {
            let model = HugeModel {
                child_id_queries: Cell::new(0),
            };
            let mut view = TreeView::from_model(model)
                .with_expanded([1, HugeModel::branch_id()])
                .with_accessible_name("Huge tree");
            view.set_viewport(720.0, f32_of_f64(ROW_HEIGHT));
            view.set_scroll_offset(f64_of_usize(HUGE_BRANCH_POSITION) * ROW_HEIGHT);

            assert_eq!(view.visible_len(), 100_000_003);
            assert_eq!(view.viewport.resident_measurements(), 0);
            assert!(view.visible_row_range().len() <= 40);
            let semantics = Widget::accessibility(&view).unwrap();
            assert_eq!(semantics.name.as_deref(), Some("Huge tree"));
            assert!(semantics.children.len() <= 40);
            assert_eq!(semantics.children[0].set_size, Some(100_000_003));
            assert!(
                view.model().child_id_queries.get() < 100,
                "one frame must query only its sparse window"
            );
        }

        // Case `delegated_tree_mounts_only_the_hundred_million_row_window`.
        {
            let builds = Rc::new(Cell::new(0_usize));
            let captured = Rc::clone(&builds);
            let mut view = TreeView::from_model(HugeModel {
                child_id_queries: Cell::new(0),
            })
            .with_expanded([1, HugeModel::branch_id()])
            .with_row_delegate(move |_, id, _| {
                captured.set(captured.get().saturating_add(1));
                Box::new(DelegateProbe::new(
                    id.to_string(),
                    22.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            });
            view.set_viewport(110.0, f32_of_f64(ROW_HEIGHT));
            view.set_scroll_offset(f64_of_usize(HUGE_BRANCH_POSITION) * ROW_HEIGHT);
            let _ = lay_out_tree(&mut view, 320.0);

            assert_eq!(view.visible_len(), 100_000_003);
            assert!(view.mounted_rows.len() <= 10);
            assert_eq!(builds.get(), view.mounted_rows.len());
            assert!(view.model().child_id_queries.get() < 100);
        }
    }

    #[test]
    fn delegated_rows_mount_measure_and_handle_input_before_selection() {
        // Case `interactive_tree_delegate_wins_before_row_selection_and_nests_semantics`.
        {
            let presses = Rc::new(Cell::new(0_usize));
            let captured = Rc::clone(&presses);
            let mut view = TreeView::new(sample_tree())
                .with_expanded([1])
                .with_row_delegate(move |model, id, _| {
                    Box::new(DelegateProbe::new(
                        model.label(id).unwrap_or("row"),
                        22.0,
                        true,
                        Rc::clone(&captured),
                    ))
                });
            view.set_viewport(66.0, f32_of_f64(ROW_HEIGHT));
            let (taffy, node) = lay_out_tree(&mut view, 260.0);

            dispatch_laid_out(&mut view, &cursor_moved(80.0, 11.0), &taffy, node);
            assert!(dispatch_laid_out(&mut view, &mouse_press(), &taffy, node));
            assert_eq!(presses.get(), 1);
            assert_eq!(view.selected(), None);

            let semantics = view.accessibility().unwrap();
            assert_eq!(semantics.children[0].role, crate::a11y::Role::TreeItem);
            assert_eq!(
                semantics.children[0].children[0].role,
                crate::a11y::Role::Button
            );
        }

        // Case `delegated_tree_measures_variable_rows_and_invalidates_only_changed_ids`.
        {
            let builds = Rc::new(Cell::new(0_usize));
            let captured = Rc::clone(&builds);
            let mut view = TreeView::from_model(external_model())
                .with_expanded([1])
                .with_row_delegate(move |model, id, _| {
                    captured.set(captured.get().saturating_add(1));
                    Box::new(DelegateProbe::new(
                        model.label(id).unwrap_or("row"),
                        if id == 2 { 55.0 } else { 22.0 },
                        false,
                        Rc::new(Cell::new(0)),
                    ))
                });
            view.set_viewport(120.0, f32_of_f64(ROW_HEIGHT));
            let _ = lay_out_tree(&mut view, 260.0);
            assert_eq!(builds.get(), 4);
            assert_eq!(view.viewport.item_height(1), Some(55.0));
            assert_eq!(view.viewport.item_top(2), Some(77.0));

            view.update_source(&[TreeModelChange::RowsChanged { ids: vec![2] }], |model| {
                model.nodes.get_mut(&2).unwrap().label = "renamed".into();
            });
            let _ = lay_out_tree(&mut view, 260.0);
            assert_eq!(builds.get(), 5, "only the invalidated id is rebuilt");
            assert_eq!(
                view.mounted_rows
                    .iter()
                    .find(|row| row.key == TreeRowKey::Node(2))
                    .and_then(|row| row.widget.accessibility())
                    .and_then(|node| node.name),
                Some("renamed".into())
            );
        }
    }

    #[test]
    fn tree_reorder_drag_previews_commits_rejects_and_hover_expands() {
        // A pointer `rows` rows down the viewport (2.5 = the middle of the
        // third row), so the cases follow the row height.
        fn row_y(rows: f64) -> f64 {
            rows * ROW_HEIGHT
        }
        // Case `tree_reorder_previews_and_commits_a_valid_sibling_drop`.
        fn tree_reorder_previews_and_commits_a_valid_sibling_drop() {
            let mut view = TreeView::from_model(external_model())
                .with_expanded([1])
                .with_reorder(
                    |_, _, _| true,
                    |model, dropped| {
                        let Some(parent) = model.parent_id(dropped.source) else {
                            return false;
                        };
                        let Some(from) = model.child_index(parent, dropped.source) else {
                            return false;
                        };
                        let Some(target_parent) = model.parent_id(dropped.target.key) else {
                            return false;
                        };
                        if target_parent != parent {
                            return false;
                        }
                        let Some(target) = model.child_index(parent, dropped.target.key) else {
                            return false;
                        };
                        let boundary = match dropped.target.placement {
                            DropPlacement::Before => target,
                            DropPlacement::After => target.saturating_add(1),
                            DropPlacement::Inside => return false,
                        };
                        let to = if boundary > from {
                            boundary.saturating_sub(1)
                        } else {
                            boundary
                        };
                        let children = &mut model.nodes.get_mut(&parent).unwrap().children;
                        let moved = children.remove(from);
                        children.insert(to, moved);
                        true
                    },
                );
            view.set_viewport(f32_of_f64(4.0 * ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
            let (taffy, node) = lay_out_tree(&mut view, 240.0);

            dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(2.5)), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
            dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(3.9)), &taffy, node);
            assert_eq!(
                view.reorder_target(),
                Some(&DropTarget::new(4, DropPlacement::After))
            );
            assert!(view.reorder_preview().is_some());
            dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);

            assert_eq!(view.model().nodes.get(&1).unwrap().children, vec![2, 4, 3]);
            assert_eq!(view.visible_index_of(3), Some(3));
            assert_eq!(view.reorder_target(), None);
        }
        tree_reorder_previews_and_commits_a_valid_sibling_drop();

        // Case `tree_reorder_rejects_a_drop_into_its_own_descendant`.
        {
            let mut view = TreeView::from_model(external_model())
                .with_expanded([1, 2])
                .with_reorder(|_, _, _| true, |_, _| true);
            view.set_viewport(f32_of_f64(5.0 * ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
            let (taffy, node) = lay_out_tree(&mut view, 240.0);

            dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(1.5)), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
            dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(2.73)), &taffy, node);
            assert!(view.reorder_preview().is_none());
            dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
            assert_eq!(view.model().nodes.get(&1).unwrap().children, vec![2, 3, 4]);
        }

        // Case `tree_reorder_hover_expands_a_collapsed_inside_target`.
        {
            let mut view = TreeView::from_model(external_model())
                .with_expanded([1])
                .with_reorder(|_, _, _| true, |_, _| false);
            view.reorder = ReorderController::new()
                .with_drag_threshold(0.0)
                .with_hover_expand_delay(Duration::ZERO);
            view.set_viewport(f32_of_f64(4.0 * ROW_HEIGHT), f32_of_f64(ROW_HEIGHT));
            let (taffy, node) = lay_out_tree(&mut view, 240.0);

            dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(2.5)), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
            dispatch_laid_out(&mut view, &cursor_moved(80.0, row_y(1.5)), &taffy, node);
            assert_eq!(
                view.reorder_target(),
                Some(&DropTarget::new(2, DropPlacement::Inside))
            );
            let state = AutoCommit::new();
            assert!(view.update(&state, &automerge::ROOT));
            assert!(view.is_expanded(2));
            assert_eq!(view.visible_index_of(20), Some(2));
        }
    }

    #[test]
    fn structural_transitions_animate_keyed_rows_unless_motion_is_reduced() {
        // Case `keyed_tree_transitions_move_survivors_and_retain_departing_rows`.
        {
            let mut view = TreeView::from_model(external_model()).with_expanded([1]);
            view.set_viewport(110.0, f32_of_f64(ROW_HEIGHT));
            let _ = lay_out_tree(&mut view, 240.0);
            view.transitions.clear();
            view.refresh_transition_snapshot();

            assert!(view.set_expanded(2, true));
            let now = crate::time::now();
            let anchor_index = view.visible_index_of(3).unwrap();
            let settled = RowGeometry::new(
                view.viewport.item_view_top(anchor_index).unwrap(),
                view.viewport.item_height(anchor_index).unwrap(),
            );
            let moving = view.transitions.visual(&TreeRowKey::Node(3), settled, now);
            assert!(moving.geometry.top < settled.top);
            let child_index = view.visible_index_of(20).unwrap();
            let entering = view.transitions.visual(
                &TreeRowKey::Node(20),
                RowGeometry::new(
                    view.viewport.item_view_top(child_index).unwrap(),
                    view.viewport.item_height(child_index).unwrap(),
                ),
                now,
            );
            assert!(entering.opacity < 1.0);
            assert!(view.transitions.is_active(now));

            view.transitions.clear();
            view.refresh_transition_snapshot();
            assert!(view.set_expanded(2, false));
            assert!(
                view.transitions
                    .exiting(crate::time::now())
                    .any(|(key, payload, visual)| {
                        *key == TreeRowKey::Node(20)
                            && payload.label == "hidden child"
                            && visual.opacity > 0.0
                    })
            );
        }

        // Case `reduced_motion_snaps_tree_structure_without_transition_state`.
        {
            let mut view = TreeView::from_model(external_model()).with_expanded([1]);
            view.set_viewport(110.0, f32_of_f64(ROW_HEIGHT));
            view.reduce_motion = true;
            view.transitions.clear();
            view.refresh_transition_snapshot();

            assert!(view.set_expanded(2, true));
            let now = crate::time::now();
            assert!(!view.transitions.is_active(now));
            assert_eq!(view.transitions.exiting(now).count(), 0);
            let child = view.visible_index_of(20).unwrap();
            let settled = RowGeometry::new(
                view.viewport.item_view_top(child).unwrap(),
                view.viewport.item_height(child).unwrap(),
            );
            assert_eq!(
                view.transitions
                    .visual(&TreeRowKey::Node(20), settled, now)
                    .opacity,
                1.0
            );
        }
    }

    // --- activation, navigator configuration, decoration -------------------

    pub(super) fn navigator_tree() -> TreeNode<()> {
        TreeNode::branch(
            1,
            "project",
            (),
            vec![
                TreeNode::branch(
                    2,
                    "src",
                    (),
                    vec![
                        TreeNode::leaf(3, "main.rs", ()),
                        TreeNode::leaf(4, "lib.rs", ()),
                    ],
                ),
                TreeNode::leaf(5, "README.md", ()),
            ],
        )
    }

    type Log = Rc<RefCell<Vec<NodeId>>>;

    fn logged(view: TreeView<()>) -> (TreeView<()>, Log, Log) {
        let activated = Log::default();
        let selected = Log::default();
        let a = Rc::clone(&activated);
        let s = Rc::clone(&selected);
        let view = view
            .on_activate(move |id| a.borrow_mut().push(id))
            .on_select(move |id| s.borrow_mut().push(id));
        (view, activated, selected)
    }

    pub(super) fn draw_view<T: 'static, M: TreeModel + 'static>(view: &mut TreeView<T, M>) {
        let mut scene = Scene::new();
        let mut text = TextRenderer::new();
        view.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
    }

    /// Centre of the visible row showing `id`.
    pub(super) fn row_center<T, M: TreeModel>(view: &TreeView<T, M>, id: NodeId) -> Point {
        let index = view.visible_index_of(id).expect("row is visible");
        let top = view.viewport.item_view_top(index).expect("row is laid out");
        let height = view.viewport.item_height(index).expect("row has a height");
        Point::new(
            view.rect.x0 + view.rect.width() * 0.75,
            view.body_top() + top + height / 2.0,
        )
    }

    fn click_row(view: &mut TreeView<()>, taffy: &TaffyTree<()>, node: taffy::NodeId, id: NodeId) {
        let at = row_center(view, id);
        dispatch_laid_out(view, &cursor_moved(at.x, at.y), taffy, node);
        dispatch_laid_out(view, &mouse_press(), taffy, node);
        dispatch_laid_out(view, &mouse_release(), taffy, node);
    }

    #[test]
    fn activation_follows_click_mode_and_semantic_activate() {
        // Case `single_click_activation_opens_on_one_click`.
        {
            let (view, activated, _) = logged(
                TreeView::new(navigator_tree())
                    .hide_root()
                    .click_activation(TreeClickActivation::SingleClick),
            );
            let mut view = view;
            let (taffy, node) = lay_out_tree(&mut view, 240.0);
            click_row(&mut view, &taffy, node, 5);
            assert_eq!(*activated.borrow(), [5]);
            assert_eq!(view.selected(), Some(5));
        }

        // Case `double_click_activation_needs_a_second_click`.
        {
            let (view, activated, _) = logged(TreeView::new(navigator_tree()).hide_root());
            let mut view = view;
            let (taffy, node) = lay_out_tree(&mut view, 240.0);
            click_row(&mut view, &taffy, node, 5);
            assert!(activated.borrow().is_empty(), "one click only selects");
            assert_eq!(view.selected(), Some(5));
            click_row(&mut view, &taffy, node, 5);
            assert_eq!(*activated.borrow(), [5], "the second click activates");
        }

        // Case `semantic_activate_selects_and_activates`.
        {
            let (mut view, activated, _) = logged(TreeView::new(navigator_tree()).hide_root());
            let mut doc = AutoCommit::new();
            assert!(Widget::perform_semantic_action(
                &mut view,
                "node:5",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut doc),
                &automerge::ROOT,
            ));
            assert_eq!(*activated.borrow(), [5]);
            assert_eq!(view.selected(), Some(5));
        }
    }

    #[test]
    fn layout_places_heading_hidden_root_and_viewport() {
        // Case `a_hidden_root_puts_its_children_at_the_top_level`.
        {
            let mut view = TreeView::new(navigator_tree()).hide_root();
            assert_eq!(
                view.visible_rows()
                    .into_iter()
                    .map(|(id, _, depth)| (id, depth))
                    .collect::<Vec<_>>(),
                [(2, 0), (5, 0)]
            );
            view.set_expanded(2, true);
            assert_eq!(
                view.visible_row(1).map(|(id, _, depth)| (id, depth)),
                Some((3, 1))
            );
            assert_eq!(view.visible_index_of(5), Some(3));
            assert_eq!(view.visible_index_of(1), None, "the root is not a row");

            view.set_focus(true);
            view.focused = Some(TreeRowKey::Node(2));
            dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowLeft)));
            dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowLeft)));
            assert_eq!(
                view.focused(),
                Some(2),
                "Left never walks onto the hidden root"
            );
        }

        // Case `the_heading_sits_above_the_rows_and_names_the_tree`.
        {
            let mut view = TreeView::new(navigator_tree())
                .hide_root()
                .with_heading("Explorer");
            let (taffy, node) = lay_out_tree(&mut view, 240.0);
            assert!(view.heading_h > 0.0);
            assert!(view.get_rect().height() >= view.heading_h + view.body_height() - 0.5);
            let in_heading = Point::new(view.rect.x0 + 40.0, view.rect.y0 + view.heading_h / 2.0);
            assert_eq!(
                view.row_key_at(in_heading),
                None,
                "the heading is not a row"
            );
            assert_eq!(
                view.row_key_at(row_center(&view, 2)),
                Some(TreeRowKey::Node(2))
            );
            dispatch_laid_out(
                &mut view,
                &cursor_moved(in_heading.x, in_heading.y),
                &taffy,
                node,
            );
            dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
            dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
            assert_eq!(
                view.selected(),
                None,
                "a press on the heading selects nothing"
            );
            let semantics = Widget::accessibility(&view).unwrap();
            assert_eq!(semantics.name.as_deref(), Some("Explorer"));
        }

        // Case `place_at_moves_paint_hit_testing_and_a_filling_viewport`.
        {
            let children = (2_u64..=40)
                .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
                .collect();
            let mut view = TreeView::new(TreeNode::branch(1, "root", (), children))
                .hide_root()
                .fill();
            view.place_at(Rect::new(-120.0, 50.0, 120.0, 250.0));
            assert!(
                (view.viewport.viewport() - 200.0).abs() < 0.01,
                "the viewport is the rect"
            );
            assert!(view.max_scroll_offset() > 0.0, "39 rows overflow 200 px");
            assert_eq!(
                view.row_key_at(Point::new(0.0, 51.0)),
                Some(TreeRowKey::Node(2)),
                "hit-testing follows the external rect"
            );
            assert_eq!(view.row_key_at(Point::new(200.0, 51.0)), None);
            view.place_at(Rect::new(0.0, 0.0, 240.0, 100.0));
            assert!(
                (view.viewport.viewport() - 100.0).abs() < 0.01,
                "and follows it each frame"
            );
        }
    }
}
