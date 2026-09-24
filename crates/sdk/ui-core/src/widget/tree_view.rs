//! `TreeView` — hierarchical list. One row per visible node, indented by
//! depth; branch nodes carry an expand/collapse glyph. Selection and focus
//! are tracked separately so arrow-key navigation can move the focus ring
//! without committing a selection until the user explicitly chooses.
//!
//! Expand/select/focus state lives on the widget; `on_select` is how the
//! caller persists a choice into its own (e.g. CRDT-backed) storage.

use crate::time::{Duration, Instant};
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;

use automerge::{AutoCommit, ObjId};
use taffy::style_helpers::{length, percent};
use taffy::{Dimension, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::collection_transition::{KeyedRowSnapshot, KeyedTransitionSet, RowGeometry};
use crate::color_compat::ColorCompat;
use crate::presence::PresenceCtx;
use crate::reorder::{DropCandidate, DropPlacement, DropTarget, ReorderController, ReorderDrop};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::colors::ACCENT_PRIMARY_IDX;
use crate::virtual_viewport::{
    DragScroller, ScrollAlignment, ScrollMomentum, SparseAnchor, SparseVirtualViewport,
};
use crate::widget::EventCtx;
use crate::widget::data_table::{
    RowType, centered_text_top, renderer_type_size, row_estimate, text_band, type_line_height,
};
use crate::widget::{Element, TaffyTreeExt, Widget};

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
}

impl SparseTreeIndex {
    fn build<M: TreeModel>(model: &M, expanded: &HashSet<NodeId>) -> Self {
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
            children.sort_unstable_by_key(|(position, _)| *position);
            children.dedup_by_key(|(position, _)| *position);
        }
        Self {
            root: Self::build_node(model, expanded, &mut grouped, root_id),
        }
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
        self.root.visible_len
    }

    fn row_at<M: TreeModel>(&self, model: &M, index: usize) -> Option<(TreeRowKey, usize)> {
        Self::row_in_node(model, &self.root, index, 0)
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
        (index < self.len()).then_some(index)
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
                let parent_index = self.index_of(model, slot.parent)?;
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
                Some(
                    parent_index
                        .saturating_add(1)
                        .saturating_add(slot.index)
                        .saturating_add(extra_before),
                )
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
    on_select: Option<Box<dyn FnMut(NodeId)>>,
    on_expand: Option<Box<dyn FnMut(NodeId, bool)>>,
    on_load_request: Option<Box<dyn FnMut(TreeLoadRequest)>>,
    reorder: ReorderController<NodeId>,
    reorder_validator: Option<TreeReorderValidator<M>>,
    reorder_commit: Option<TreeReorderCommit<M>>,
    drag_scroll: DragScroller,
    momentum: ScrollMomentum,
    last_tick: Option<Instant>,
    transition_snapshot: Vec<KeyedRowSnapshot<TreeRowKey, TreeTransitionPayload>>,
    transitions: KeyedTransitionSet<TreeRowKey, TreeTransitionPayload>,
    transition_duration: Duration,
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
}

/// Matches `theme.tokens.spacing.s4` (16.0); also the per-depth indent unit.
const DISCLOSURE_WIDTH: f64 = 16.0;
/// Indent per depth before a theme is seen: the `s4` step, unscaled.
#[allow(
    clippy::cast_lossless,
    reason = "const context: f64::from is not const; f32 -> f64 is exact"
)]
const DEFAULT_INDENT: f64 = crate::tokens::Spacing::BASE.s4 as f64;
const ROW_HEIGHT: f64 = 22.0;
const WHEEL_LINE_PX: f64 = 60.0;
const MAX_TRANSITION_ROWS: usize = 512;

fn idx_to_f64(index: usize) -> f64 {
    f64::from(u32::try_from(index).unwrap_or(u32::MAX))
}

type TreeRowDelegate<M> = Box<dyn FnMut(&M, NodeId, usize) -> Element>;
type TreeSlotDelegate<M> =
    Box<dyn for<'a> FnMut(&M, TreeSlot, usize, TreeSlotState<'a>) -> Element>;
type TreeReorderValidator<M> = Box<dyn FnMut(&M, NodeId, &DropTarget<NodeId>) -> bool>;
type TreeReorderCommit<M> = Box<dyn FnMut(&mut M, ReorderDrop<NodeId>) -> bool>;

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

    pub fn replace_model(&mut self, root: TreeNode<T>) {
        let anchor = self.capture_view_anchor();
        self.model = root;
        self.invalidate_all_delegates();
        self.reconcile_model_state(anchor.as_ref());
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
        let index = SparseTreeIndex::build(&model, &expanded);
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
            on_select: None,
            on_expand: None,
            on_load_request: None,
            reorder: ReorderController::new(),
            reorder_validator: None,
            reorder_commit: None,
            drag_scroll: DragScroller::new(),
            momentum: ScrollMomentum::new(),
            last_tick: None,
            transition_snapshot: Vec::new(),
            transitions: KeyedTransitionSet::new(),
            transition_duration: Duration::from_millis(210),
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
            index,
            viewport,
            last_anchor: None,
            interaction: crate::interaction::InteractionEnvironment::default(),
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
        if !self.model.is_branch(id) {
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

    fn reconcile_model_state(&mut self, anchor: Option<&SparseAnchor<TreeRowKey>>) {
        let anchor = anchor.map(|anchor| self.normalize_anchor(anchor.clone()));
        self.expanded
            .retain(|id| self.model.contains(*id) && self.model.is_branch(*id));
        self.selection.reconcile(|id| self.model.contains(*id));
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
        let old_snapshot = self.transition_snapshot.clone();
        let next = SparseTreeIndex::build(&self.model, &self.expanded);
        self.viewport.reconcile(
            next.len(),
            |key| next.index_of_key(&self.model, *key),
            anchor,
        );
        self.index = next;
        let new_snapshot = self.capture_transition_snapshot();
        self.transitions.reconcile(
            &old_snapshot,
            &new_snapshot,
            crate::time::now(),
            self.transition_duration,
            crate::animation::Curve::EaseInOut,
            self.reduce_motion,
        );
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
        px(self.scroll_offset())
    }

    #[must_use]
    pub fn scroll_offset(&self) -> f64 {
        self.viewport.offset()
    }

    #[must_use]
    pub fn max_scroll_y(&self) -> f32 {
        px(self.max_scroll_offset())
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
        let line = type_line_height(tokens.typography.scale.sm);
        self.disclosure_w = text_band(DISCLOSURE_WIDTH, line, 0.0);
        self.indent_px = f64::from(tokens.spacing.s4);
        let pad = f64::from(tokens.spacing.s1);
        self.row_type = Some(RowType {
            line,
            default_pad: pad,
            authored_pad: pad,
        });
        self.refresh_row_estimate();
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
        let local_y = cursor.y - self.rect.y0;
        let index = self.viewport.hit_test(local_y)?;
        let (TreeRowKey::Node(id), depth) = self.index.row_at(&self.model, index)? else {
            return None;
        };
        let top = self.viewport.item_view_top(index)?;
        let height = self.viewport.item_height(index)?.max(1.0);
        let fraction = ((local_y - top) / height).clamp(0.0, 1.0);
        let placement = if fraction < 0.25 {
            DropPlacement::Before
        } else if fraction > 0.75 || !self.model.is_branch(id) {
            DropPlacement::After
        } else {
            DropPlacement::Inside
        };
        let indent = idx_to_f64(depth) * self.indent_px;
        let preview = match placement {
            DropPlacement::Before => Rect::new(
                self.rect.x0 + indent,
                self.rect.y0 + top - 1.0,
                self.rect.x1,
                self.rect.y0 + top + 1.0,
            ),
            DropPlacement::After => Rect::new(
                self.rect.x0 + indent,
                self.rect.y0 + top + height - 1.0,
                self.rect.x1,
                self.rect.y0 + top + height + 1.0,
            ),
            DropPlacement::Inside => Rect::new(
                self.rect.x0 + indent,
                self.rect.y0 + top,
                self.rect.x1,
                self.rect.y0 + top + height,
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
        let index = self.viewport.hit_test(cursor.y - self.rect.y0)?;
        let (TreeRowKey::Node(id), depth) = self.index.row_at(&self.model, index)? else {
            return None;
        };
        let glyph_x0 = self.rect.x0 + idx_to_f64(depth) * self.indent_px;
        (!(self.model.is_branch(id)
            && cursor.x >= glyph_x0
            && cursor.x < glyph_x0 + self.disclosure_w))
            .then_some(id)
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
        let changed = self
            .reorder_commit
            .as_mut()
            .is_some_and(|commit| commit(&mut self.model, dropped));
        if changed {
            self.reconcile_model_state(anchor.as_ref());
        }
        changed
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
        let Some(index) = self.viewport.hit_test(pt.y - self.rect.y0) else {
            return false;
        };
        let Some((key, depth)) = self.index.row_at(&self.model, index) else {
            return false;
        };
        let glyph_x0 = self.rect.x0 + idx_to_f64(depth) * self.indent_px;
        match key {
            TreeRowKey::Node(id) => {
                let is_branch = self.model.is_branch(id);
                if is_branch && pt.x >= glyph_x0 && pt.x < glyph_x0 + self.disclosure_w {
                    self.toggle_expand(id);
                } else {
                    self.apply_selection_gesture(id, toggle, extend);
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

    fn on_wheel(&mut self, delta: &MouseScrollDelta) -> bool {
        if self.viewport.viewport() <= 0.0 {
            return false;
        }
        let dy = match delta {
            MouseScrollDelta::LineDelta(_, y) if !self.reduce_motion => {
                self.momentum.push(f64::from(*y) * -WHEEL_LINE_PX)
            }
            MouseScrollDelta::LineDelta(_, y) => {
                self.momentum.stop();
                f64::from(*y) * -WHEEL_LINE_PX
            }
            MouseScrollDelta::PixelDelta(position) => {
                self.momentum.stop();
                -position.y
            }
        };
        self.last_tick = Some(crate::time::now());
        let before = self.viewport.offset();
        self.set_scroll_offset(before + dy);
        let moved = self.viewport.offset().to_bits() != before.to_bits();
        if !moved {
            self.momentum.stop();
        }
        moved
    }

    fn focus_at(&mut self, index: usize) -> bool {
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
            Key::Named(NamedKey::Enter) => self.activate_focus(),
            Key::Character(text) if text == " " => self.activate_focus(),
            Key::Character(text)
                if self.multi_select
                    && toggle
                    && (text.as_str() == "a" || text.as_str() == "A") =>
            {
                self.select_all();
                true
            }
            _ => false,
        }
    }

    fn activate_focus(&mut self) -> bool {
        let Some(key) = self.focused else {
            return false;
        };
        match key {
            TreeRowKey::Node(id) => {
                self.select(id);
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

    #[allow(
        clippy::too_many_arguments,
        reason = "forwards the Widget event contract plus its handled/focus outputs"
    )]
    fn route_to_delegates(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<taffy::NodeId>,
        taffy: &TaffyTree<()>,
    ) {
        if let Some(row) = self
            .mounted_rows
            .iter_mut()
            .find(|row| row.widget.wants_pointer_capture())
            && let Some(node) = row.node
        {
            row.widget.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                node,
                taffy,
            );
            return;
        }
        for row in self.mounted_rows.iter_mut().rev() {
            let Some(node) = row.node else {
                continue;
            };
            row.widget.handle_event(
                event,
                scale_factor,
                &modifiers,
                state,
                path,
                handled,
                focus_target,
                node,
                taffy,
            );
            if *handled {
                break;
            }
        }
    }
}

#[inline]
#[allow(
    clippy::cast_possible_truncation,
    reason = "bounded logical-pixel geometry is represented as f32 by the widget API"
)]
fn px(value: f64) -> f32 {
    value as f32
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
    }

    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.reconcile_mounted_rows();
        for row in &mut self.mounted_rows {
            row.widget.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut AutoCommit, path: &ObjId) -> bool {
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
        if self.momentum.is_active() {
            let delta = self.momentum.tick(elapsed);
            let before = self.scroll_offset();
            self.set_scroll_offset(before + delta);
            let moved = self.scroll_offset().to_bits() != before.to_bits();
            changed |= moved;
            if !moved {
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
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        self.transition_duration = theme.tokens.motion.durations.base();
        self.reduce_motion = theme.reduce_motion;
        if self.reduce_motion {
            self.transitions.clear();
            self.momentum.stop();
        }
        self.apply_type_metrics(theme);
        self.reconcile_mounted_rows();
        let height = self.body_height();
        let disclosure_w = self.disclosure_w;
        let indent_px = self.indent_px;
        let mut children = Vec::with_capacity(self.mounted_rows.len());
        for row in &mut self.mounted_rows {
            let child = row.widget.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(child).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            style.inset.left = length(px(idx_to_f64(row.depth) * indent_px + disclosure_w));
            style.inset.right = length(0.0_f32);
            style.inset.top = length(px(self
                .viewport
                .item_view_top(row.index)
                .unwrap_or_default()));
            style.size.width = Dimension::auto();
            let _ = taffy.set_style(child, style);
            row.node = Some(child);
            children.push(child);
        }
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: Dimension::length(px(height)),
                },
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
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let child_origin = Point::new(self.rect.x0, self.rect.y0);
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
        self.refresh_transition_snapshot();
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
        self.request_visible_rows();
        let tokens = &theme.tokens;
        let text_size = renderer_type_size(text_renderer, tokens.typography.scale.sm);
        // Every label and disclosure glyph is one line of this style, so one
        // measured line box centres every row identically.
        let (_, line_h) = text_renderer.measure("\u{25B6}", text_size, None);
        // The indent and disclosure column hit-testing uses, resolved from
        // this theme at layout.
        let indent_px = self.indent_px;
        let accent = tokens.colors.accent[ACCENT_PRIMARY_IDX];
        let now = crate::time::now();

        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );

        let visible = self.visible_row_range();
        for i in visible {
            let Some((key, depth)) = self.index.row_at(&self.model, i) else {
                continue;
            };
            let (label, text_color, is_branch) = match key {
                TreeRowKey::Node(id) => {
                    let Some(label) = self.model.label(id) else {
                        continue;
                    };
                    (label, tokens.colors.text_primary, self.model.is_branch(id))
                }
                TreeRowKey::Slot(slot) => match self.model.child_state(slot.parent, slot.index) {
                    TreeChildState::Ready(_) => continue,
                    TreeChildState::Loading => ("Loading\u{2026}", tokens.colors.text_dim, false),
                    TreeChildState::Error { message } => (
                        message.unwrap_or("Could not load. Activate to retry"),
                        tokens.colors.semantic.danger,
                        false,
                    ),
                },
            };
            let Some(view_top) = self.viewport.item_view_top(i) else {
                continue;
            };
            let Some(row_height) = self.viewport.item_height(i) else {
                continue;
            };
            let visual = self
                .transitions
                .visual(&key, RowGeometry::new(view_top, row_height), now);
            let row_height = visual.geometry.height;
            let opacity = visual.opacity;
            let y0 = self.rect.y0 + visual.geometry.top;
            let y1 = (y0 + row_height).min(self.rect.y0 + self.body_height());
            if y1 <= self.rect.y0 {
                continue;
            }
            if y0 >= self.rect.y0 + self.body_height() {
                break;
            }
            let row_rect = Rect::new(self.rect.x0, y0, self.rect.x1, y1);

            if matches!(key, TreeRowKey::Node(id) if self.selection.is_selected(&id)) {
                crate::widget::data_table::paint_selected_row(
                    scene, transform, theme, row_rect, opacity,
                );
            }

            if self.is_focused && self.focused == Some(key) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    tokens.colors.border_focus.with_alpha_factor(opacity),
                    None,
                    &row_rect,
                );
            }

            let depth_px = idx_to_f64(depth) * indent_px;
            let glyph_x = self.rect.x0 + depth_px;
            let label_x = glyph_x + self.disclosure_w;
            let text_y = centered_text_top(y0, row_height, line_h);

            if is_branch {
                let TreeRowKey::Node(id) = key else {
                    continue;
                };
                let glyph = if self.expanded.contains(&id) {
                    "\u{25BC}"
                } else {
                    "\u{25B6}"
                };
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "kurbo Affine composition is floating-point matrix math; it cannot overflow or panic"
                )]
                let glyph_xf = transform * Affine::translate((glyph_x, text_y));
                text_renderer.draw_text(
                    scene,
                    glyph,
                    text_size,
                    tokens.colors.text_dim.with_alpha_factor(opacity),
                    glyph_xf,
                    None,
                );
            }

            let delegated = self.mounted_rows.iter().any(|row| row.key == key);
            if !delegated {
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "kurbo Affine composition is floating-point matrix math; it cannot overflow or panic"
                )]
                let label_xf = transform * Affine::translate((label_x, text_y));
                text_renderer.draw_text(
                    scene,
                    label,
                    text_size,
                    text_color.with_alpha_factor(opacity),
                    label_xf,
                    None,
                );
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
            let y0 = self.rect.y0 + visual.geometry.top;
            let row_rect = Rect::new(self.rect.x0, y0, self.rect.x1, y0 + visual.geometry.height);
            if payload.selected {
                crate::widget::data_table::paint_selected_row(
                    scene, transform, theme, row_rect, opacity,
                );
            }
            let glyph_x = self.rect.x0 + idx_to_f64(payload.depth) * indent_px;
            let text_y = centered_text_top(y0, visual.geometry.height, line_h);
            if payload.is_branch {
                let glyph = if payload.expanded { "▼" } else { "▶" };
                #[allow(
                    clippy::arithmetic_side_effects,
                    reason = "kurbo Affine composition is finite screen-space matrix math"
                )]
                let glyph_transform = transform * Affine::translate((glyph_x, text_y));
                text_renderer.draw_text(
                    scene,
                    glyph,
                    text_size,
                    tokens.colors.text_dim.with_alpha_factor(opacity),
                    glyph_transform,
                    None,
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
            text_renderer.draw_text(
                scene,
                &payload.label,
                text_size,
                color.with_alpha_factor(opacity),
                label_transform,
                None,
            );
        }
        if let Some(preview) = self.reorder.preview() {
            let color = if self
                .reorder
                .target()
                .is_some_and(|target| target.placement == DropPlacement::Inside)
            {
                accent.with_alpha_factor(0.25)
            } else {
                accent
            };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                color,
                None,
                &preview,
            );
        }
        scene.pop_layer();
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keeps delegate, reorder, disclosure, selection, scroll, and keyboard precedence explicit"
    )]
    fn handle_event_ctx(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let scale_factor = ctx.scale_factor;
        let modifiers = ctx.modifiers;
        let state = &mut *ctx.state;
        let path = ctx.path;
        let handled = &mut *ctx.handled;
        let focus_target = &mut *ctx.focus_target;
        let node_id = ctx.node_id;
        let taffy = ctx.taffy;
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = Some(Point::new(
                position.x / scale_factor,
                position.y / scale_factor,
            ));
        }
        if !self.reorder.is_tracking() && !self.drag_scroll.is_tracking() {
            self.route_to_delegates(
                event,
                scale_factor,
                *modifiers,
                state,
                path,
                handled,
                focus_target,
                taffy,
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
                let source = (self.reorder_validator.is_some() && self.reorder_commit.is_some())
                    .then(|| self.reorder_source_at_cursor())
                    .flatten();
                let toggle = modifiers.state().control_key() || modifiers.state().super_key();
                let extend = modifiers.state().shift_key();
                if let (Some(source), Some(cursor)) = (source, self.last_cursor) {
                    *handled = self.on_left_press(node_id, focus_target, toggle, extend);
                    self.reorder.begin(source, cursor);
                    self.last_tick = Some(crate::time::now());
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
            WindowEvent::MouseWheel { delta, .. }
                if self
                    .last_cursor
                    .is_some_and(|point| self.rect.contains(point)) =>
            {
                self.drag_scroll.cancel();
                *handled = self.on_wheel(delta);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.momentum.stop();
                if *logical_key == Key::Named(NamedKey::Escape)
                    && (self.reorder.cancel() || self.drag_scroll.cancel())
                {
                    *handled = true;
                } else {
                    let toggle = modifiers.state().control_key() || modifiers.state().super_key();
                    let extend = modifiers.state().shift_key();
                    *handled = self.on_key_with_modifiers(logical_key, toggle, extend);
                }
            }
            _ => {}
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
        visible
            .contains(&absolute)
            .then(|| absolute.saturating_sub(visible.start))
    }

    fn wants_pointer_capture(&self) -> bool {
        self.reorder.is_tracking()
            || self.drag_scroll.is_tracking()
            || self
                .mounted_rows
                .iter()
                .any(|row| row.widget.wants_pointer_capture())
    }

    fn needs_animation_frame(&self) -> bool {
        self.momentum.is_active()
            || self.model.needs_poll()
            || self.reorder.is_active()
            || self.transitions.is_active(crate::time::now())
            || self
                .mounted_rows
                .iter()
                .any(|row| row.widget.needs_animation_frame())
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
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the complete virtual tree semantic hierarchy is constructed in one auditable pass"
    )]
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        // Labels are runtime strings supplied by the host, not FTL keys.
        let value = self
            .selected()
            .and_then(|id| self.model.label(id))
            .map(str::to_owned);
        let mut node = AccessNode::new(Role::Tree)
            .with_optional_name(
                self.accessible_name
                    .as_deref()
                    .map(crate::i18n::I18nManager::lookup_plain),
            )
            .with_optional_value(value)
            .with_state(AccessState {
                focusable: self.is_focusable(),
                focused: self.is_focused(),
                disabled: !self.is_enabled(),
                ..Default::default()
            });
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
        let row_count = self.index.len();
        let children = self
            .visible_row_range()
            .filter_map(|index| {
                let (key, depth) = self.index.row_at(&self.model, index)?;
                let delegate_child = self
                    .mounted_rows
                    .iter()
                    .find(|row| row.key == key)
                    .and_then(|row| row.widget.accessibility());
                let delegate_label = delegate_child.as_ref().and_then(|child| child.name.clone());
                let (label, expanded, busy, invalid, selected) = match key {
                    TreeRowKey::Node(id) => (
                        self.model.label(id)?.to_owned(),
                        self.model
                            .is_branch(id)
                            .then(|| self.expanded.contains(&id)),
                        false,
                        false,
                        self.selection.is_selected(&id),
                    ),
                    TreeRowKey::Slot(slot) => {
                        match self.model.child_state(slot.parent, slot.index) {
                            TreeChildState::Ready(_) => return None,
                            TreeChildState::Loading => (
                                delegate_label.unwrap_or_else(|| "Loading".to_owned()),
                                None,
                                true,
                                false,
                                false,
                            ),
                            TreeChildState::Error { message } => {
                                let label = delegate_label.unwrap_or_else(|| {
                                    format!(
                                        "{}. Activate to retry",
                                        message.unwrap_or("Could not load")
                                    )
                                });
                                (label, None, false, true, false)
                            }
                        }
                    }
                };
                let mut item = AccessNode::new(Role::TreeItem)
                    .with_action_target(match key {
                        TreeRowKey::Node(id) => format!("node:{id}"),
                        TreeRowKey::Slot(slot) => {
                            format!("slot:{}:{}", slot.parent, slot.index)
                        }
                    })
                    .with_name(label)
                    .with_position(index.saturating_add(1), row_count)
                    .with_level(depth.saturating_add(1))
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.is_focused && self.focused == Some(key),
                        selected,
                        expanded,
                        disabled: !self.is_enabled(),
                        busy,
                        invalid,
                        ..Default::default()
                    });
                if let (Some(top), Some(height)) = (
                    self.viewport.item_view_top(index),
                    self.viewport.item_height(index),
                ) {
                    item = item.with_bounds(Rect::new(
                        self.rect.x0,
                        self.rect.y0 + top,
                        self.rect.x1,
                        self.rect.y0 + top + height,
                    ));
                }
                if let Some(child) = delegate_child {
                    item = item.with_children(vec![child]);
                }
                Some(item)
            })
            .collect();
        node = node.with_children(children);
        Some(node)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        use crate::a11y::Action;

        if let Some(raw) = target.strip_prefix("node:") {
            let Ok(id) = raw.parse::<NodeId>() else {
                return false;
            };
            if !self.model.contains(id) {
                return false;
            }
            match action {
                Action::Focus => {
                    let Some(index) = self.visible_index_of(id) else {
                        return false;
                    };
                    self.focus_at(index)
                }
                Action::Activate => {
                    self.focused = Some(TreeRowKey::Node(id));
                    self.select(id);
                    true
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
                    let Some(index) = self.visible_index_of(id) else {
                        return false;
                    };
                    self.viewport
                        .scroll_to_index(index, ScrollAlignment::Nearest);
                    self.mark_delegate_window_dirty();
                    true
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
                    self.focused = Some(TreeRowKey::Slot(slot));
                    self.activate_focus()
                }
                _ => false,
            }
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use taffy::TaffyTree;
    use taffy::prelude::TaffyMaxContent;
    use winit::event::{KeyEvent, Modifiers};
    use winit::keyboard::{KeyLocation, PhysicalKey};

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
    fn owned_tree_reorder_moves_complete_subtrees_across_parents() {
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

    #[test]
    fn owned_tree_reorder_rejects_cycles_invalid_targets_and_noops() {
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

    #[test]
    fn owned_tree_reorderable_installs_safe_default_controller() {
        let view = TreeView::new(reorder_tree()).reorderable();
        assert!(view.reorder_validator.is_some());
        assert!(view.reorder_commit.is_some());
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
        fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<taffy::NodeId, *mut dyn Widget>,
        ) -> taffy::NodeId {
            let node = taffy.leaf_node(Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: Dimension::length(self.height),
                },
                ..Default::default()
            });
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: taffy::NodeId, origin: Point) {
            let layout = taffy.layout(node).copied().unwrap_or_default();
            self.rect = Rect::new(
                origin.x + f64::from(layout.location.x),
                origin.y + f64::from(layout.location.y),
                origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
                origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
            );
        }

        fn draw(
            &mut self,
            _scene: &mut Scene,
            _transform: Affine,
            _text_renderer: &mut TextRenderer,
            _theme: &Theme,
            _presence: &PresenceCtx,
        ) {
        }

        fn handle_event_ctx(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
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

    fn lay_out_tree<T: 'static, M: TreeModel + 'static>(
        view: &mut TreeView<T, M>,
        width: f32,
    ) -> (TaffyTree<()>, taffy::NodeId) {
        let mut doc = AutoCommit::new();
        view.sync(&mut doc, &automerge::ROOT);
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = view.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(width),
                    height: taffy::AvailableSpace::Definite(px(view.body_height())),
                },
            )
            .unwrap();
        view.apply_layout(&taffy, node, Point::ZERO);
        (taffy, node)
    }

    fn cursor_moved(x: f64, y: f64) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(x, y),
        }
    }

    fn mouse_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    fn mouse_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    fn key_press(logical_key: Key) -> WindowEvent {
        // SAFETY: KeyEvent has private fields in winit 0.30; the public
        // ones tolerate the zero bit pattern and `handle_event` only
        // inspects `logical_key` + `state`.
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }

    fn dispatch(view: &mut TreeView<()>, event: &WindowEvent) {
        let mut tree = TaffyTree::<()>::new();
        let node = tree.leaf_node(Style::default());
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        view.handle_event(
            event,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &root,
            &mut handled,
            &mut focus_target,
            node,
            &tree,
        );
    }

    fn dispatch_laid_out<T: 'static, M: TreeModel + 'static>(
        view: &mut TreeView<T, M>,
        event: &WindowEvent,
        taffy: &TaffyTree<()>,
        node: taffy::NodeId,
    ) -> bool {
        let mut handled = false;
        let mut focus_target = None;
        let mut doc = AutoCommit::new();
        view.handle_event(
            event,
            1.0,
            &Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus_target,
            node,
            taffy,
        );
        handled
    }

    /// At 2× text the disclosure column is painted at the `sm` line width
    /// (30 px), not the 1× 16 px. A press between the two must toggle the
    /// branch: the hit zone is the painted column, not the pinned constant.
    #[test]
    fn disclosure_hit_zone_is_the_painted_column_at_two_x() {
        let root = TreeNode::branch(1, "crates", (), vec![TreeNode::leaf(2, "main.rs", ())]);
        let mut view = TreeView::new(root);
        let mut doc = AutoCommit::new();
        view.sync(&mut doc, &automerge::ROOT);
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let theme = Theme::dark().with_text_scale(2.0);
        let node = view.build_layout(&mut taffy, &theme, &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: taffy::AvailableSpace::Definite(320.0),
                    height: taffy::AvailableSpace::Definite(px(view.body_height())),
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

    #[test]
    fn tree_view_expand_node_shows_children() {
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

    #[test]
    fn tree_view_select_node_calls_on_select() {
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
                size: Size {
                    width: Dimension::length(200.0),
                    height: Dimension::length(80.0),
                },
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
            idx_to_f64(view.visible_rows().len()) * ROW_HEIGHT,
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

    #[test]
    fn multiselect_uses_stable_ids_and_survives_collapse_and_move() {
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

    #[test]
    fn multiselect_supports_visible_ranges_and_constant_space_select_all() {
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

    #[test]
    fn tree_view_arrow_right_expands_focused_node() {
        let mut view = TreeView::new(sample_tree());
        view.focused = Some(TreeRowKey::Node(1));
        assert!(!view.is_expanded(1));

        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowRight)));

        assert!(
            view.is_expanded(1),
            "ArrowRight must expand the focused branch"
        );
    }

    #[test]
    fn tree_view_arrow_down_moves_focus_to_next_visible_row() {
        let mut view = TreeView::new(sample_tree());
        view.toggle_expand(1);
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

    #[test]
    fn accessibility_reports_tree_role_and_no_value_when_unselected() {
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

    #[test]
    fn tree_view_exposes_an_explicit_stable_id() {
        let view = TreeView::new(sample_tree()).with_id("workspace-tree");
        assert_eq!(Widget::test_id(&view), Some("workspace-tree"));
    }

    #[test]
    fn tree_viewport_builder_enables_the_bounded_internal_window() {
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

    #[test]
    fn accessibility_value_is_selected_row_label() {
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

    #[test]
    fn accessibility_reflects_focus_state() {
        let mut view = TreeView::new(sample_tree());
        view.set_focus(true);
        let node = Widget::accessibility(&view).unwrap();
        assert!(
            node.state.focused,
            "widget focus surfaces in the access node"
        );
    }

    #[test]
    fn empty_branch_remains_expandable_for_lazy_population() {
        let mut view = TreeView::new(TreeNode::branch(1, "loading", (), Vec::new()));
        view.toggle_expand(1);
        assert!(view.is_expanded(1));
        let node = Widget::accessibility(&view).unwrap();
        assert_eq!(node.children[0].state.expanded, Some(true));
    }

    #[test]
    fn expansion_has_a_persistable_builder_and_change_callback() {
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

    #[test]
    fn batched_model_updates_reconcile_lazy_children_and_deleted_selection() {
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

    #[test]
    fn expanding_above_viewport_preserves_the_stable_row_anchor() {
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
        view.set_viewport(px(ROW_HEIGHT), px(ROW_HEIGHT));
        view.set_scroll_y(px(ROW_HEIGHT * 2.0 + 5.0));
        let before = view.viewport.item_view_top(2);

        view.toggle_expand(2);

        let anchor = view.visible_index_of(10).expect("anchor remains visible");
        assert_eq!(view.viewport.item_view_top(anchor), before);
    }

    #[test]
    fn large_tree_bounds_paint_and_accessibility_windows() {
        let children = (2_u64..=100_001)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
        view.toggle_expand(1);
        view.set_viewport(110.0, px(ROW_HEIGHT));
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

    #[test]
    fn semantic_tree_rows_publish_scrolled_viewport_geometry() {
        let mut view = TreeView::new(sample_tree());
        view.toggle_expand(1);
        view.set_viewport(px(ROW_HEIGHT * 2.0), px(ROW_HEIGHT));
        view.set_scroll_y(10.0);
        let _ = lay_out_tree(&mut view, 320.0);

        let semantics = Widget::accessibility(&view).expect("tree semantics");
        let first = semantics.children.first().expect("first visible row");
        assert_eq!(
            first.bounds.expect("tree row bounds").to_rect(),
            Rect::new(0.0, -10.0, 320.0, ROW_HEIGHT - 10.0),
        );
    }

    #[test]
    fn page_navigation_and_accelerated_wheel_share_sparse_row_geometry() {
        let children = (2_u64..=101)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
        view.toggle_expand(1);
        view.set_viewport(100.0, px(ROW_HEIGHT));
        assert!(view.set_row_height(0, 300.0));
        assert!(view.focus_at(0));

        assert!(view.on_key_with_modifiers(&Key::Named(NamedKey::PageDown), false, false,));
        assert_eq!(view.focused(), Some(1));
        assert_eq!(view.scroll_y(), 90.0);
        assert!(view.on_key_with_modifiers(&Key::Named(NamedKey::PageUp), false, false,));
        assert_eq!(view.scroll_y(), 0.0);

        assert!(view.on_wheel(&MouseScrollDelta::LineDelta(0.0, -1.0)));
        let immediate = view.scroll_y();
        assert_eq!(immediate, 60.0);
        assert!(view.needs_animation_frame());
        view.last_tick = Some(
            crate::time::now()
                .checked_sub(Duration::from_millis(16))
                .expect("a 16 ms earlier instant is representable"),
        );
        assert!(Widget::update(
            &mut view,
            &mut AutoCommit::new(),
            &automerge::ROOT,
        ));
        assert!(view.scroll_y() > immediate, "momentum advances after input");
    }

    #[test]
    fn body_drag_scrolls_tree_without_selecting_but_click_still_selects() {
        let children = (2_u64..=101)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
        view.toggle_expand(1);
        view.set_viewport(100.0, px(ROW_HEIGHT));
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

    #[test]
    fn tree_items_publish_level_expansion_selection_and_focus() {
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

    #[test]
    fn keyboard_navigation_reveals_focus_and_follows_tree_parents() {
        let children = (2_u64..=20)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
        view.toggle_expand(1);
        view.set_viewport(44.0, px(ROW_HEIGHT));
        view.focused = Some(TreeRowKey::Node(1));

        dispatch(&mut view, &key_press(Key::Named(NamedKey::End)));
        assert_eq!(view.focused(), Some(20));
        assert!(view.scroll_y() > 0.0, "End reveals the final row");

        dispatch(&mut view, &key_press(Key::Named(NamedKey::ArrowLeft)));
        assert_eq!(view.focused(), Some(1), "Left on a leaf focuses its parent");
        assert_eq!(view.scroll_y(), 0.0, "revealing the root returns to top");
    }

    #[test]
    fn external_model_queries_only_expanded_branches_and_preserves_insert_anchor() {
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

        view.set_viewport(px(ROW_HEIGHT), px(ROW_HEIGHT));
        view.set_scroll_y(px(ROW_HEIGHT * 2.0 + 5.0));
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

    #[test]
    fn post_mutation_notification_uses_the_cached_stable_anchor() {
        let children = Rc::new(RefCell::new(vec![2, 3, 4]));
        let mut view = TreeView::from_model(SharedOrderModel {
            children: Rc::clone(&children),
        })
        .with_expanded([1]);
        view.set_viewport(px(ROW_HEIGHT), px(ROW_HEIGHT));
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

    #[test]
    fn granular_external_changes_refresh_data_and_reconcile_removed_state() {
        let mut view = TreeView::from_model(external_model())
            .with_expanded([1])
            .with_selected(Some(3));
        view.focused = Some(TreeRowKey::Node(3));
        view.set_viewport(66.0, px(ROW_HEIGHT));
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

    #[test]
    fn external_move_keeps_measurements_attached_to_node_ids() {
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

    #[test]
    fn paged_tree_demands_only_the_visible_window_once() {
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
        view.set_viewport(66.0, px(ROW_HEIGHT));
        view.set_scroll_offset(idx_to_f64(50_000_000) * ROW_HEIGHT);

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

    #[test]
    fn model_owned_paged_tree_drains_slot_completions_during_update() {
        let mut view = TreeView::from_model(PagedModel {
            count: 4,
            states: Rc::new(RefCell::new(HashMap::new())),
            self_loading: true,
            requests: Vec::new(),
            changes: Vec::new(),
        })
        .with_expanded([1]);
        view.set_viewport(66.0, px(ROW_HEIGHT));
        assert!(matches!(
            view.index.row_at(&view.model, 1).map(|(key, _)| key),
            Some(TreeRowKey::Slot(_))
        ));
        assert!(view.focus_at(1));

        let demanded = view.request_visible_rows();
        assert!(demanded > 0);
        assert_eq!(view.model().requests.len(), demanded);

        let mut document = AutoCommit::new();
        assert!(Widget::update(&mut view, &mut document, &automerge::ROOT));

        let first = PagedModel::id(0).expect("stable paged id");
        assert_eq!(
            view.index.row_at(&view.model, 1).map(|(key, _)| key),
            Some(TreeRowKey::Node(first))
        );
        assert_eq!(view.focused(), Some(first));
        assert_eq!(view.visible_row(1), Some((first, "loaded row", 1)));
        assert_eq!(view.request_visible_rows(), 0);
    }

    #[test]
    fn paged_error_is_keyboard_retryable_and_ready_row_inherits_focus_and_anchor() {
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
        view.set_viewport(px(ROW_HEIGHT), px(ROW_HEIGHT));
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

    #[test]
    fn paged_slot_delegate_localizes_measures_and_rebuilds_changed_placeholders() {
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
        view.set_viewport(120.0, px(ROW_HEIGHT));
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

    #[test]
    fn delegated_tree_mounts_only_the_hundred_million_row_window() {
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
        view.set_viewport(110.0, px(ROW_HEIGHT));
        view.set_scroll_offset(idx_to_f64(HUGE_BRANCH_POSITION) * ROW_HEIGHT);
        let _ = lay_out_tree(&mut view, 320.0);

        assert_eq!(view.visible_len(), 100_000_003);
        assert!(view.mounted_rows.len() <= 10);
        assert_eq!(builds.get(), view.mounted_rows.len());
        assert!(view.model().child_id_queries.get() < 100);
    }

    #[test]
    fn interactive_tree_delegate_wins_before_row_selection_and_nests_semantics() {
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
        view.set_viewport(66.0, px(ROW_HEIGHT));
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

    #[test]
    fn delegated_tree_measures_variable_rows_and_invalidates_only_changed_ids() {
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
        view.set_viewport(120.0, px(ROW_HEIGHT));
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

    #[test]
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
        view.set_viewport(88.0, px(ROW_HEIGHT));
        let (taffy, node) = lay_out_tree(&mut view, 240.0);

        dispatch_laid_out(&mut view, &cursor_moved(80.0, 55.0), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &cursor_moved(80.0, 86.0), &taffy, node);
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

    #[test]
    fn owned_tree_reorderable_commits_the_pointer_drop_without_app_bookkeeping() {
        let mut view = TreeView::new(sample_tree())
            .with_expanded([1])
            .reorderable();
        view.set_viewport(88.0, px(ROW_HEIGHT));
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

    #[test]
    fn tree_reorder_rejects_a_drop_into_its_own_descendant() {
        let mut view = TreeView::from_model(external_model())
            .with_expanded([1, 2])
            .with_reorder(|_, _, _| true, |_, _| true);
        view.set_viewport(110.0, px(ROW_HEIGHT));
        let (taffy, node) = lay_out_tree(&mut view, 240.0);

        dispatch_laid_out(&mut view, &cursor_moved(80.0, 33.0), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &cursor_moved(80.0, 60.0), &taffy, node);
        assert!(view.reorder_preview().is_none());
        dispatch_laid_out(&mut view, &mouse_release(), &taffy, node);
        assert_eq!(view.model().nodes.get(&1).unwrap().children, vec![2, 3, 4]);
    }

    #[test]
    fn tree_reorder_hover_expands_a_collapsed_inside_target() {
        let mut view = TreeView::from_model(external_model())
            .with_expanded([1])
            .with_reorder(|_, _, _| true, |_, _| false);
        view.reorder = ReorderController::new()
            .with_drag_threshold(0.0)
            .with_hover_expand_delay(Duration::ZERO);
        view.set_viewport(88.0, px(ROW_HEIGHT));
        let (taffy, node) = lay_out_tree(&mut view, 240.0);

        dispatch_laid_out(&mut view, &cursor_moved(80.0, 55.0), &taffy, node);
        dispatch_laid_out(&mut view, &mouse_press(), &taffy, node);
        dispatch_laid_out(&mut view, &cursor_moved(80.0, 33.0), &taffy, node);
        assert_eq!(
            view.reorder_target(),
            Some(&DropTarget::new(2, DropPlacement::Inside))
        );
        let mut state = AutoCommit::new();
        assert!(view.update(&mut state, &automerge::ROOT));
        assert!(view.is_expanded(2));
        assert_eq!(view.visible_index_of(20), Some(2));
    }

    #[test]
    fn keyed_tree_transitions_move_survivors_and_retain_departing_rows() {
        let mut view = TreeView::from_model(external_model()).with_expanded([1]);
        view.set_viewport(110.0, px(ROW_HEIGHT));
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

    #[test]
    fn reduced_motion_snaps_tree_structure_without_transition_state() {
        let mut view = TreeView::from_model(external_model()).with_expanded([1]);
        view.set_viewport(110.0, px(ROW_HEIGHT));
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

    #[test]
    fn reduced_motion_keeps_direct_scroll_without_wheel_or_drag_decay() {
        let children = (2_u64..102)
            .map(|id| TreeNode::leaf(id, format!("row {id}"), ()))
            .collect();
        let mut view = TreeView::new(TreeNode::branch(1, "root", (), children));
        view.set_expanded(1, true);
        view.set_viewport(100.0, 22.0);
        view.reduce_motion = true;

        assert!(view.on_wheel(&MouseScrollDelta::LineDelta(0.0, -1.0)));
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

    #[test]
    fn sparse_tree_index_resolves_a_hundred_million_children() {
        let model = HugeModel {
            child_id_queries: Cell::new(0),
        };
        let branch = HugeModel::branch_id();
        let expanded = HashSet::from([1, branch]);
        let index = SparseTreeIndex::build(&model, &expanded);

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
        let after_branch = HugeModel::direct_id(HUGE_BRANCH_POSITION.saturating_add(1)).unwrap();
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

    #[test]
    fn tree_view_virtualizes_a_hundred_million_child_model_end_to_end() {
        let model = HugeModel {
            child_id_queries: Cell::new(0),
        };
        let mut view = TreeView::from_model(model)
            .with_expanded([1, HugeModel::branch_id()])
            .with_accessible_name("Huge tree");
        view.set_viewport(720.0, px(ROW_HEIGHT));
        view.set_scroll_offset(idx_to_f64(HUGE_BRANCH_POSITION) * ROW_HEIGHT);

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
}
