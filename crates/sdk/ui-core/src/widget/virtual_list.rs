//! A sparse, variable-height list: only the rows near the viewport own widgets, and
//! those survive a reorder while their key stays visible.

use crate::time::Instant;
use crate::widget::{AppModel, ModelPath};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::Hash;
use std::ops::Range;

use taffy::style_helpers::percent;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use std::cell::Cell;
use std::rc::Rc;

use crate::arriving::Arrival;
use crate::presence::PresenceCtx;
use crate::reorder::{DropCandidate, DropPlacement, DropTarget, ReorderController, ReorderDrop};
use crate::selection::KeySelection;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::virtual_viewport::{
    DragScroller, ScrollAlignment, SparseAnchor, SparseVirtualViewport, WheelStep,
};
use crate::widget::rows::RowStep;
use crate::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
use crate::widget::{ArrivingRows, Element, TaffyTreeExt, Widget};

#[path = "virtual_list_groups.rs"]
mod groups;

#[path = "virtual_list_vec_model.rs"]
mod vec_model;
use groups::Groups;
pub use vec_model::VecListModel;
#[path = "virtual_list_remount.rs"]
mod remount;

#[path = "virtual_list_choice.rs"]
mod choice;
#[path = "virtual_list_reconcile.rs"]
mod reconcile;
#[path = "virtual_list_shown.rs"]
mod shown;
use shown::ListShown;
#[path = "virtual_list_tree.rs"]
mod tree;
pub use tree::{TREE_INDENT, TreeMarks, TreeMove, TreeRow};
#[path = "virtual_list_menu.rs"]
mod menu;
use menu::RowMenus;
#[path = "virtual_list_edit.rs"]
mod edit;
pub use edit::RowRenameField;
#[path = "virtual_list_jump.rs"]
mod jump;
#[path = "virtual_list_reorder.rs"]
mod reordering;
use edit::{RenameCallback, RenameField};
#[path = "virtual_list_lift.rs"]
pub(super) mod lift;
#[path = "virtual_list_press.rs"]
mod press;
#[path = "virtual_list_row_group.rs"]
mod row_group;
pub use row_group::RowGroup;

const DEFAULT_VIEWPORT_HEIGHT: f64 = 320.0;
const END_SLACK: f64 = 0.5;

fn fit_needs_layout(box_height: f64, rows: f64, viewport: f64) -> bool {
    (box_height - rows).abs() > 0.5 && (viewport - rows).abs() > 0.5
}

fn px(value: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "screen-space logical pixels are bounded far below f32::MAX"
    )]
    let value = value as f32;
    value
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListModelChange<K> {
    RowsInserted {
        index: usize,
        count: usize,
    },
    RowsRemoved {
        index: usize,
        count: usize,
    },
    /// `to` is the first destination index in the post-move order.
    RowsMoved {
        from: usize,
        to: usize,
        count: usize,
    },
    RowsChanged {
        keys: Vec<K>,
    },
    Reset,
}

/// A collection a [`VirtualList`] reads rows from. Keys are unique and stay stable
/// while they name the same item.
pub trait ListModel {
    type Key: Clone + Eq + Hash + 'static;

    fn len(&self) -> usize;
    fn key_at(&self, index: usize) -> Option<Self::Key>;
    fn index_of(&self, key: &Self::Key) -> Option<usize>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn row_state(&self, _index: usize) -> Arrival<(), &str> {
        Arrival::Ready(())
    }

    fn can_retry(&self, _index: usize) -> bool {
        false
    }

    fn retry(&mut self, _index: usize) {}

    fn request_range(&mut self, _range: Range<usize>, _open: Option<&Self::Key>) {}

    fn poll_changed(&mut self) -> bool {
        false
    }

    fn poll_changes(&mut self) -> Vec<ListModelChange<Self::Key>> {
        Vec::new()
    }

    fn needs_poll(&self) -> bool {
        false
    }

    fn label(&self, _index: usize) -> Option<String> {
        None
    }

    fn key_presenting(&self, _id: &str) -> Option<Self::Key> {
        None
    }

    fn item_presented(&self, _key: &Self::Key) -> Option<String> {
        None
    }

    fn group_at(&self, _index: usize) -> Option<RowGroup> {
        None
    }

    fn done(&self, _index: usize) -> bool {
        false
    }

    fn tree_at(&self, _index: usize) -> Option<TreeRow<Self::Key>> {
        None
    }

    fn accepts_children(&self, _index: usize) -> bool {
        false
    }

    fn renames(&self, _index: usize) -> bool {
        true
    }

    fn has_subtasks(&self, _index: usize) -> bool {
        false
    }

    fn discloses(&self) -> bool {
        false
    }

    fn opens_in_place(&self, _index: usize) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DuplicateListKey;

impl fmt::Display for DuplicateListKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("list model keys must be unique")
    }
}

impl std::error::Error for DuplicateListKey {}

type RowBuilder<M> = Box<dyn FnMut(&M, usize, &<M as ListModel>::Key, RowBuild) -> Element>;
type KeyCallback<K> = Box<dyn FnMut(K)>;
type FoldCallback<K> = Box<dyn FnMut(&K, bool)>;
type SemanticKey<K> = Box<dyn Fn(&K) -> String>;
type RowPresence<M> =
    Box<dyn Fn(&M, usize, &<M as ListModel>::Key) -> Option<crate::presence::PresenceKey>>;
type SelectCallback<K> = Box<dyn FnMut(K, bool)>;
type SelectedCallback<K> = Box<dyn FnMut(&[K])>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Choosing {
    ByHand,
    Following,
}
type ReorderValidator<M> =
    Box<dyn FnMut(&M, &<M as ListModel>::Key, &DropTarget<<M as ListModel>::Key>) -> bool>;
type ReorderCommit<M> = Box<dyn FnMut(&mut M, ReorderDrop<<M as ListModel>::Key>) -> bool>;
use super::row_selection::selection_sink;
pub use super::row_selection::{
    PaintedRowChevrons, PaintedRowLifts, RowBuild, RowChevron, RowContentBox, RowLift,
    RowSelection, RowSelectionSink, row_content_box,
};

struct MountedRow<K> {
    key: K,
    index: usize,
    stamp: u64,
    state: MountedRowState,
    selection: RowSelection,
    widget: Element,
    node: Option<NodeId>,
    replacing: Option<LaidOutRow>,
}

struct LaidOutRow {
    widget: Element,
    node: NodeId,
}

impl<K> MountedRow<K> {
    fn laid_out(self) -> Option<LaidOutRow> {
        match self.node {
            Some(node) => Some(LaidOutRow {
                widget: self.widget,
                node,
            }),
            None => self.replacing,
        }
    }

    fn laid_out_node(&self) -> Option<NodeId> {
        self.node
            .or_else(|| self.replacing.as_ref().map(|row| row.node))
    }

    fn laid_out_widget(&self) -> &dyn Widget {
        match (&self.node, &self.replacing) {
            (None, Some(row)) => row.widget.as_ref(),
            _ => self.widget.as_ref(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MountedRowState {
    Ready,
    Pending,
    Failed(String, bool),
}

pub trait ListEdges {
    fn theme(&mut self, theme: &Theme);

    fn edges(&mut self, width: f64) -> (f64, f64);
}

pub trait RowActionRects {
    fn row_action_rects(&self) -> Vec<Rect>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowStance {
    Bleed,
    Flush,
    Column,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "focus, drag threshold, fetch, structural, and layout invalidation are independent widget states"
)]
#[crate::component::not_a_component(
    part,
    "`ui.VirtualList` (`DescribedList`) is its described form"
)]
pub struct VirtualList<M: ListModel> {
    test_id: Option<String>,
    model: M,
    row_builder: RowBuilder<M>,
    selection_aware: bool,
    row_presence: Option<RowPresence<M>>,
    tooltips_suppressed: bool,
    delegate_selection: bool,
    viewport: SparseVirtualViewport<M::Key>,
    mounted: Vec<MountedRow<M::Key>>,
    /// Rows let go of whose nodes the retained layout still holds: they keep its walk's
    /// place, and their widgets alive, until a remount unmounts them.
    letting_go: Vec<(usize, LaidOutRow)>,
    selection: KeySelection<M::Key>,
    multi_select: bool,
    reading: bool,
    follow_focus: bool,
    focused_key: Option<M::Key>,
    /// The group's heading holding the keyboard, with the index of the first row under it.
    focused_group: Option<(String, usize)>,
    typing: crate::type_ahead::TypeAhead,
    on_select: Option<SelectCallback<M::Key>>,
    on_selected: Option<SelectedCallback<M::Key>>,
    reported_many: bool,
    view: Option<(crate::widget::view_choice::ViewChoice, KeyCallback<M::Key>)>,
    opens: Option<(crate::widget::view_choice::ViewChoice, KeyCallback<M::Key>)>,
    on_activate: Option<KeyCallback<M::Key>>,
    open_on_press: bool,
    on_fold: Option<FoldCallback<M::Key>>,
    on_opened: Option<KeyCallback<M::Key>>,
    on_closed: Option<Box<dyn FnMut()>>,
    open: Option<M::Key>,
    rereading_open: Option<M::Key>,
    held_open: Option<String>,
    growth: Vec<(M::Key, crate::motion::MotionValue<f64>)>,
    growing: HashSet<M::Key>,
    menus: RowMenus<M>,
    clicks: super::rows::Clicks<M::Key>,
    semantic_key: Option<SemanticKey<M::Key>>,
    accessible_name: Option<String>,
    rect: Rect,
    last_cursor: Option<Point>,
    is_focused: bool,
    drag_scroll: DragScroller,
    pressed_row: Option<(usize, M::Key)>,
    reorder: ReorderController<M::Key>,
    reorder_validator: Option<ReorderValidator<M>>,
    reorder_commit: Option<ReorderCommit<M>>,
    scroll: super::rows::RowScroll,
    indent_guides: bool,
    motion: crate::tokens::EffectiveMotion,
    feel: crate::motion::Feel,
    reduce_motion: bool,
    vbar: Scrollbar,
    overlay_bar: Option<crate::widget::OverlayScrollbar>,
    gutter: f64,
    corners: crate::widget::scrollbar::SurfaceCorners,
    bleed: f64,
    lead: f64,
    chevron_column: f64,
    disclosure: f64,
    lift: f64,
    painted_lifts: Vec<crate::widget::RowLift>,
    painted_chevrons: Vec<crate::widget::virtual_list::RowChevron>,
    in_place: bool,
    row_line: f64,
    stance: RowStance,
    edges: Option<Box<dyn ListEdges>>,
    content_gutter: f64,
    rows_beside_bar: bool,
    fill_height: bool,
    fit_rows: bool,
    follow_end: bool,
    at_end: bool,
    last_tick: Option<Instant>,
    last_anchor: Option<SparseAnchor<M::Key>>,
    requested_range: Option<Range<usize>>,
    requested_open: Option<M::Key>,
    request_dirty: bool,
    structure_dirty: bool,
    box_dirty: bool,
    layout_dirty: bool,
    scroll_dirty: bool,
    interaction: crate::interaction::InteractionEnvironment,
    /// The scroll offset the rows' layout was built at: a scroll that brings no row near
    /// moves them without a rebuild.
    layout_offset: f64,
    mounted_range: Range<usize>,
    remount: bool,
    invalidated: HashSet<M::Key>,
    row_builds: u64,
    access_rows: std::cell::RefCell<a11y::KeptAccessRows<M::Key>>,
    layout_node: Option<NodeId>,
    shown: crate::scroll_copy::ShownFrames<ListShown>,
    row_motion: crate::collection_transition::RowMotions<M::Key>,
    arrivals: ArrivingRows<M::Key>,
    retry_asked: Rc<Cell<Option<usize>>>,
    placed: Option<Range<usize>>,
    near_placed: Range<usize>,
    unread: Option<String>,
    jump_hovered: bool,
    jump_pressed: bool,
    pill_height: f64,
    pill_width: f64,
    names_item: std::cell::RefCell<Option<Box<dyn Fn(&str) -> Option<M::Key>>>>,
    groups: Groups,
    tail_title: Option<String>,
    view_title: Option<String>,
    view_count: Option<usize>,
    heads_nothing: std::cell::Cell<Option<bool>>,
    row_groups: HashMap<M::Key, Option<groups::Group>>,
    tree: Option<Rc<TreeMarks<M::Key>>>,
    tree_rows: HashMap<M::Key, TreeRow<M::Key>>,
    tree_hidden: HashSet<M::Key>,
    first_level_branch: Option<bool>,
    column_rows: HashSet<M::Key>,
    rename: Option<Box<RenameField<M::Key>>>,
    on_rename: Option<RenameCallback<M::Key>>,
    rename_on_double_click: bool,
    tab_indents: bool,
    on_delete: Option<KeyCallback<M::Key>>,
}

impl<M: ListModel> VirtualList<M> {
    pub fn new(
        model: M,
        estimated_row_height: f32,
        mut row_builder: impl FnMut(&M, usize, &M::Key) -> Element + 'static,
    ) -> Self {
        Self::build(
            model,
            estimated_row_height,
            Box::new(move |model, index, key, _| row_builder(model, index, key)),
            false,
        )
    }

    pub fn with_selection_delegate(
        model: M,
        estimated_row_height: f32,
        row_builder: impl FnMut(&M, usize, &M::Key, RowBuild) -> Element + 'static,
    ) -> Self {
        let mut list = Self::build(model, estimated_row_height, Box::new(row_builder), true);
        list.delegate_selection = true;
        list
    }

    fn build(
        model: M,
        estimated_row_height: f32,
        row_builder: RowBuilder<M>,
        selection_aware: bool,
    ) -> Self {
        let mut viewport = SparseVirtualViewport::new(f64::from(estimated_row_height));
        viewport.set_viewport(DEFAULT_VIEWPORT_HEIGHT);
        viewport.set_overscan(viewport.estimated_height() * 2.0);
        viewport.reconcile(model.len(), |key| model.index_of(key), None);
        let mut list = Self {
            test_id: None,
            model,
            row_builder,
            selection_aware,
            row_presence: None,
            tooltips_suppressed: false,
            delegate_selection: false,
            viewport,
            mounted: Vec::new(),
            letting_go: Vec::new(),
            selection: KeySelection::new(),
            multi_select: false,
            reading: false,
            follow_focus: false,
            focused_key: None,
            focused_group: None,
            typing: crate::type_ahead::TypeAhead::default(),
            on_select: None,
            on_selected: None,
            reported_many: false,
            view: None,
            opens: None,
            on_activate: None,
            open_on_press: false,
            on_fold: None,
            on_opened: None,
            on_closed: None,
            open: None,
            rereading_open: None,
            held_open: None,
            growth: Vec::new(),
            growing: HashSet::new(),
            menus: RowMenus::None,
            row_builds: 0,
            access_rows: std::cell::RefCell::default(),
            indent_guides: false,
            clicks: super::rows::Clicks::default(),
            semantic_key: None,
            accessible_name: None,
            rect: Rect::ZERO,
            last_cursor: None,
            is_focused: false,
            drag_scroll: DragScroller::new(),
            pressed_row: None,
            reorder: ReorderController::new(),
            reorder_validator: None,
            reorder_commit: None,
            scroll: super::rows::RowScroll::default(),
            motion: crate::tokens::EffectiveMotion::default(),
            feel: crate::motion::Feel::Standard,
            reduce_motion: false,
            vbar: Scrollbar::vertical(),
            overlay_bar: None,
            gutter: f64::from(crate::tokens::Metrics::BASE.scrollbar_gutter),
            corners: crate::widget::scrollbar::SurfaceCorners::default(),
            bleed: f64::from(crate::tokens::Spacing::BASE.s2),
            lead: f64::from(crate::tokens::Spacing::BASE.s2),
            chevron_column: 0.0,
            disclosure: f64::from(
                crate::tokens::Metrics::BASE.icon_sm + crate::tokens::Spacing::BASE.rhythm.related,
            ),
            lift: f64::from(crate::tokens::Spacing::BASE.s2),
            painted_lifts: Vec::new(),
            painted_chevrons: Vec::new(),
            in_place: true,
            row_line: f64::from(crate::tokens::Metrics::BASE.row_single),
            stance: RowStance::Bleed,
            edges: None,
            content_gutter: 0.0,
            rows_beside_bar: false,
            fill_height: false,
            fit_rows: false,
            follow_end: false,
            at_end: false,
            last_tick: None,
            last_anchor: None,
            requested_range: None,
            requested_open: None,
            request_dirty: true,
            structure_dirty: true,
            box_dirty: false,
            layout_dirty: true,
            interaction: crate::interaction::InteractionEnvironment::default(),
            layout_offset: 0.0,
            mounted_range: 0..0,
            remount: false,
            invalidated: HashSet::new(),
            scroll_dirty: false,
            layout_node: None,
            shown: crate::scroll_copy::ShownFrames::default(),
            row_motion: crate::collection_transition::RowMotions::default(),
            arrivals: ArrivingRows::new(),
            retry_asked: Rc::new(Cell::new(None)),
            placed: None,
            near_placed: 0..0,
            unread: None,
            jump_hovered: false,
            jump_pressed: false,
            pill_height: 0.0,
            pill_width: 0.0,
            names_item: std::cell::RefCell::new(None),
            groups: Groups::default(),
            tail_title: None,
            view_title: None,
            view_count: None,
            heads_nothing: std::cell::Cell::new(None),
            row_groups: HashMap::new(),
            tree: None,
            tree_rows: HashMap::new(),
            tree_hidden: HashSet::new(),
            first_level_branch: None,
            column_rows: HashSet::new(),
            rename: None,
            on_rename: None,
            rename_on_double_click: false,
            tab_indents: false,
            on_delete: None,
        };
        list.refresh_anchor();
        list
    }

    pub fn reveal(&mut self, key: &M::Key) -> bool {
        let Some(index) = self.model.index_of(key) else {
            return false;
        };
        self.focus_row(key.clone());
        let now = crate::motion::frame_now();
        let width = self.rect.width();
        match self
            .scroll
            .reveal(&mut self.viewport, index, width, self.motion, now)
        {
            Some(target) => self.set_scroll_offset(target),
            None => self.follow_scroll_to(now),
        }
        self.visible_window_changed();
        true
    }

    fn follow_scroll_to(&mut self, now: Instant) {
        if let Some(at) = self.scroll.follow(now) {
            self.set_scroll_offset(at);
        }
    }

    fn reveal_in_progress(&self, now: Instant) -> bool {
        self.scroll.to.target().is_some()
            || self.scroll.to.is_moving(now)
            || self.shown.drawing().is_some_and(|shown| shown.in_motion)
    }

    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_feel(mut self, feel: crate::motion::Feel) -> Self {
        self.feel = feel;
        self
    }

    pub fn set_feel(&mut self, feel: crate::motion::Feel) {
        if self.feel != feel {
            self.feel = feel;
            self.layout_dirty = true;
        }
    }

    #[must_use]
    pub fn with_viewport_height(mut self, height: f32) -> Self {
        self.set_viewport_height(height);
        self
    }

    pub fn set_viewport_height(&mut self, height: f32) {
        self.viewport.set_viewport(f64::from(height));
        self.keep_end();
        self.visible_window_changed();
    }

    #[must_use]
    pub fn overlay_scrollbar(mut self) -> Self {
        self.overlay_bar = Some(crate::widget::OverlayScrollbar::vertical());
        self
    }

    #[must_use]
    pub const fn flush(mut self) -> Self {
        self.stance = RowStance::Flush;
        self
    }

    #[must_use]
    pub const fn in_column(mut self) -> Self {
        self.stance = RowStance::Column;
        self
    }

    #[must_use]
    pub fn with_edges(mut self, edges: impl ListEdges + 'static) -> Self {
        self.edges = Some(Box::new(edges));
        self
    }

    fn update_edges(&mut self) -> bool {
        let width = (self.rect.width() - self.lead - self.trailing_inset()).max(0.0);
        let Some(edges) = self.edges.as_mut() else {
            return self.viewport.set_edges(self.content_gutter, 0.0);
        };
        let (lead, trail) = edges.edges(width);
        self.viewport.set_edges(lead, trail)
    }

    fn trailing_inset(&self) -> f64 {
        let bar = if self.rows_beside_bar {
            self.gutter
        } else {
            0.0
        };
        match self.stance {
            RowStance::Column => self.bleed.max(bar + self.lift),
            RowStance::Bleed | RowStance::Flush => self.lead + bar,
        }
    }

    #[must_use]
    pub fn filling(mut self) -> Self {
        self.fill_height = true;
        self
    }

    #[must_use]
    pub fn fitting_rows(mut self) -> Self {
        self.fill_height = false;
        self.fit_rows = true;
        self
    }

    #[must_use]
    pub fn multi_select(mut self, enabled: bool) -> Self {
        self.multi_select = enabled;
        self
    }

    pub fn set_multi_select(&mut self, enabled: bool) {
        self.multi_select = enabled;
    }

    #[must_use]
    pub fn on_selected(mut self, callback: impl FnMut(&[M::Key]) + 'static) -> Self {
        self.set_on_selected(callback);
        self
    }

    pub fn set_on_selected(&mut self, callback: impl FnMut(&[M::Key]) + 'static) {
        self.on_selected = Some(Box::new(callback));
    }

    #[must_use]
    pub fn selection_follows_focus(mut self) -> Self {
        self.follow_focus = true;
        self
    }

    /// The rows are read, not chosen: a press or a tap gives a row the keyboard and
    /// chooses nothing, and a conversation's messages are read this way.
    #[must_use]
    pub const fn reading(mut self) -> Self {
        self.reading = true;
        self
    }

    #[must_use]
    pub fn with_selection(mut self, selection: KeySelection<M::Key>) -> Self {
        self.set_selection(selection);
        self
    }

    pub fn set_selection(&mut self, mut selection: KeySelection<M::Key>) {
        selection.reconcile(|key| self.model.index_of(key).is_some());
        self.reported_many = selection.is_select_all() || selection.stored_keys().count() > 1;
        self.selection = selection;
    }

    pub fn selection_len(&self) -> usize {
        self.selection.len(self.model.len())
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    #[must_use]
    pub fn with_row_presence(
        mut self,
        presence: impl Fn(&M, usize, &M::Key) -> Option<crate::presence::PresenceKey> + 'static,
    ) -> Self {
        self.row_presence = Some(Box::new(presence));
        self
    }

    #[must_use]
    pub fn on_select(mut self, callback: impl FnMut(M::Key, bool) + 'static) -> Self {
        self.set_on_select(callback);
        self
    }

    pub fn set_on_select(&mut self, callback: impl FnMut(M::Key, bool) + 'static) {
        self.on_select = Some(Box::new(callback));
    }

    #[must_use]
    pub fn on_activate(mut self, callback: impl FnMut(M::Key) + 'static) -> Self {
        self.set_on_activate(callback);
        self
    }

    pub fn set_on_activate(&mut self, callback: impl FnMut(M::Key) + 'static) {
        self.on_activate = Some(Box::new(callback));
    }

    #[must_use]
    pub fn on_opened(mut self, callback: impl FnMut(M::Key) + 'static) -> Self {
        self.set_on_opened(callback);
        self
    }

    pub fn set_on_opened(&mut self, callback: impl FnMut(M::Key) + 'static) {
        self.on_opened = Some(Box::new(callback));
    }

    #[must_use]
    pub fn on_closed(mut self, callback: impl FnMut() + 'static) -> Self {
        self.set_on_closed(callback);
        self
    }

    pub fn set_on_closed(&mut self, callback: impl FnMut() + 'static) {
        self.on_closed = Some(Box::new(callback));
    }

    #[must_use]
    pub fn on_fold(mut self, callback: impl FnMut(&M::Key, bool) + 'static) -> Self {
        self.on_fold = Some(Box::new(callback));
        self
    }

    /// Encodes a key as an opaque, stable string for accessibility and headless row
    /// actions; it must not change while the key names the same item.
    #[must_use]
    pub fn with_semantic_key(mut self, encode: impl Fn(&M::Key) -> String + 'static) -> Self {
        self.semantic_key = Some(Box::new(encode));
        self
    }

    fn key_for_name(&self, encoded: &str) -> Option<M::Key> {
        let encode = self.semantic_key.as_ref()?;
        (0..self.model.len())
            .filter_map(|index| self.model.key_at(index))
            .find(|key| encode(key) == encoded)
    }

    #[must_use]
    pub fn with_reorder(
        mut self,
        validate: impl FnMut(&M, &M::Key, &DropTarget<M::Key>) -> bool + 'static,
        commit: impl FnMut(&mut M, ReorderDrop<M::Key>) -> bool + 'static,
    ) -> Self {
        self.reorder_validator = Some(Box::new(validate));
        self.reorder_commit = Some(Box::new(commit));
        self
    }

    pub fn suppress_tooltips(&mut self, suppressed: bool) {
        if self.tooltips_suppressed == suppressed {
            return;
        }
        self.tooltips_suppressed = suppressed;
        self.apply_row_tooltips();
    }

    fn apply_row_tooltips(&mut self) {
        let suppressed = self.tooltips_suppressed;
        for row in &mut self.mounted {
            if let Some(list_row) = row
                .widget
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<crate::widget::ListRow>())
            {
                list_row.suppress_tooltips(suppressed);
            }
        }
    }
    pub fn reorder_target(&self) -> Option<&DropTarget<M::Key>> {
        self.reorder.target()
    }

    #[must_use]
    pub fn reorder_preview(&self) -> Option<Rect> {
        self.reorder.preview()
    }

    #[must_use]
    pub fn model(&self) -> &M {
        &self.model
    }

    pub fn focus_row(&mut self, key: M::Key) {
        self.focused_group = None;
        self.focused_key = Some(key);
    }

    fn focused_row_node(&self) -> Option<NodeId> {
        self.mounted
            .iter()
            .find(|row| Some(&row.key) == self.focused_key.as_ref() && row.widget.is_focusable())
            .and_then(|row| row.node)
    }

    fn note_scrolled(&mut self) {
        if self.follow_end {
            self.at_end = self.viewport.max_offset() - self.viewport.offset() <= END_SLACK;
        }
    }

    fn keep_end(&mut self) -> bool {
        if !(self.follow_end && self.at_end) {
            return false;
        }
        let end = self.viewport.max_offset();
        if self.viewport.offset().to_bits() == end.to_bits() {
            return false;
        }
        self.viewport.set_offset(end);
        true
    }

    fn scroll_to_index(&mut self, index: usize) {
        self.viewport
            .scroll_to_index(index, ScrollAlignment::Nearest);
        self.note_scrolled();
        self.visible_window_changed();
    }

    #[must_use]
    pub fn selection(&self) -> &KeySelection<M::Key> {
        &self.selection
    }

    pub fn selection_mut(&mut self) -> &mut KeySelection<M::Key> {
        &mut self.selection
    }

    #[must_use]
    pub fn focused_key(&self) -> Option<&M::Key> {
        self.focused_key.as_ref()
    }

    #[must_use]
    pub fn mounted_len(&self) -> usize {
        self.mounted.len()
    }

    pub fn mounted_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.mounted.iter().map(|row| row.index)
    }

    #[must_use]
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        self.viewport.visible_range()
    }

    fn rows_to_hold(&self) -> std::ops::Range<usize> {
        let near = self.viewport.visible_range();
        let count = self.model.len();
        near.start.min(count)..near.end.min(count)
    }

    pub fn set_scroll_offset(&mut self, offset: f64) {
        self.viewport.set_offset(offset);
        self.note_scrolled();
        self.scroll_dirty = true;
        self.refresh_anchor();
    }

    fn chrome_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.is_focused.hash(&mut hasher);
        self.focused_key.hash(&mut hasher);
        self.delegate_selection.hash(&mut hasher);
        self.reorder.is_active().hash(&mut hasher);
        self.selection.is_select_all().hash(&mut hasher);
        let selected = self.selection.stored_keys().fold(0_u64, |combined, key| {
            let mut one = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut one);
            combined ^ one.finish()
        });
        selected.hash(&mut hasher);
        hasher.finish()
    }

    fn key_hash(key: &M::Key) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    fn first_shown_row(&self) -> Option<(usize, u64, f64)> {
        let give = self.give();
        self.mounted.iter().find_map(|row| {
            let top = self.viewport.item_view_top(row.index)? - give;
            let height = self.viewport.item_height(row.index)?;
            (top + height > 0.0).then(|| (row.index, Self::key_hash(&row.key), top))
        })
    }

    fn places_near(&self) -> Vec<(u64, f64, f64)> {
        self.mounted
            .get(self.mounted_near())
            .unwrap_or_default()
            .iter()
            .filter_map(|row| {
                let top = self.viewport.item_top(row.index)?;
                let height = self.viewport.item_height(row.index)?;
                Some((Self::key_hash(&row.key), top, height))
            })
            .collect()
    }

    fn headings_drawn(&self) -> Vec<(String, Option<usize>, bool)> {
        self.groups
            .bands
            .iter()
            .map(|band| {
                (
                    band.heading.clone(),
                    self.shown_heading_count(band),
                    band.closed,
                )
            })
            .collect()
    }

    fn moved_since_presented(&self) -> Option<f64> {
        let (index, key, top) = self.shown.presented()?.first_shown?;
        let still = self
            .model
            .key_at(index)
            .is_some_and(|at| Self::key_hash(&at) == key);
        if !still {
            return None;
        }
        Some(self.viewport.item_view_top(index)? - self.give() - top)
    }

    fn give(&self) -> f64 {
        self.scroll.momentum.give(crate::motion::frame_now())
    }

    #[must_use]
    pub fn scroll_offset(&self) -> f64 {
        self.viewport.offset()
    }

    #[must_use]
    pub fn max_scroll_offset(&self) -> f64 {
        self.viewport.max_offset()
    }

    fn scroll_metrics(&self) -> ScrollMetrics {
        ScrollMetrics::new(
            self.viewport.content_extent(),
            self.viewport.viewport(),
            self.viewport.offset(),
        )
    }

    fn scrollbar_track(&self) -> Option<Rect> {
        if !self.scroll_metrics().overflows() {
            return None;
        }
        if let Some(bar) = &self.overlay_bar {
            return Some(bar.track(self.rect));
        }
        let track = self.corners.clear(
            crate::widget::scrollbar::vertical_track(self.rect, self.gutter),
            true,
        );
        (track.y1 > track.y0).then_some(track)
    }

    #[must_use]
    pub fn scrollbar_thumb(&self) -> Option<Rect> {
        self.vbar
            .thumb_rect(self.scrollbar_track()?, self.scroll_metrics())
    }

    pub fn set_row_height(&mut self, index: usize, height: f32) -> bool {
        let Some(key) = self.model.key_at(index) else {
            return false;
        };
        let changed = self.viewport.measure(index, key, f64::from(height));
        if changed {
            self.visible_window_changed();
        }
        changed
    }

    fn row_at_cursor(&self) -> Option<(usize, M::Key)> {
        self.row_at(self.last_cursor?)
    }

    pub fn key_at_point(&self, point: Point) -> Option<M::Key> {
        self.row_at(point).map(|(_, key)| key)
    }

    fn row_at(&self, cursor: Point) -> Option<(usize, M::Key)> {
        if !self.rect.contains(cursor) {
            return None;
        }
        let index = self
            .viewport
            .hit_test(cursor.y - self.rect.y0 - self.end_slack())?;
        let row = self.row_box(index)?;
        if cursor.y < row.y0 || cursor.y >= row.y1 {
            return None;
        }
        matches!(self.model.row_state(index), Arrival::Ready(()))
            .then(|| self.model.key_at(index).map(|key| (index, key)))
            .flatten()
    }

    fn reorder_candidate(&mut self, cursor: Point) -> Option<DropCandidate<M::Key>> {
        if self.model.is_empty() || self.rect.height() <= 0.0 {
            return None;
        }
        let local_y =
            (cursor.y - self.shown_top(0.0)).clamp(0.0, self.rect.height() - f64::EPSILON);
        let index = self.viewport.hit_test(local_y)?;
        if !matches!(self.model.row_state(index), Arrival::Ready(())) {
            return None;
        }
        let key = self.model.key_at(index)?;
        if self.tree.is_some() {
            let row = self.row_box(index)?;
            return self.tree_candidate(index, &key, row, cursor);
        }
        let top = self.viewport.item_view_top(index)?;
        let height = self.viewport.item_height(index)?.max(1.0);
        let before = local_y < top + height / 2.0;
        let y = self.shown_top(if before { top } else { top + height });
        Some(DropCandidate {
            target: DropTarget::new(
                key,
                if before {
                    DropPlacement::Before
                } else {
                    DropPlacement::After
                },
            ),
            preview: Rect::new(self.rect.x0, y - 1.0, self.rect.x1, y + 1.0),
            can_hover_expand: false,
        })
    }

    fn focus_index(&mut self, index: usize, extend: bool) -> bool {
        let Some(key) = self.model.key_at(index) else {
            return false;
        };
        self.focus_row(key.clone());
        self.select_focused(key, extend);
        self.scroll_to_index(index);
        true
    }

    fn select_focused(&mut self, key: M::Key, extend: bool) {
        if extend {
            self.apply_selection(key, false, true, Choosing::ByHand);
        } else if self.follow_focus && !self.selection.is_selected(&key) {
            self.apply_selection(key, false, false, Choosing::Following);
        }
    }

    fn page_focus(&mut self, forward: bool, extend: bool) -> bool {
        let count = self.model.len();
        let current = self
            .focused_key
            .as_ref()
            .and_then(|key| self.model.index_of(key))
            .unwrap_or_else(|| if forward { 0 } else { count.saturating_sub(1) });
        let Some((index, scrolled)) = self.viewport.page_from(current, forward) else {
            return false;
        };
        let Some(key) = self.model.key_at(index) else {
            return false;
        };
        self.focus_row(key.clone());
        self.select_focused(key, extend);
        self.visible_window_changed();
        scrolled || index != current
    }

    fn on_key(&mut self, key: &Key, toggle: bool, extend: bool) -> bool {
        let count = self.model.len();
        if count == 0 {
            return false;
        }
        let current = self.keyboard_row();
        if let Some(moved) = self.move_stop(key, extend) {
            return moved;
        }
        if let Some(RowStep::Page(forward)) = super::rows::row_step(key, current, count) {
            return self.page_focus(forward, extend);
        }
        if !toggle && !extend {
            match key {
                Key::Named(NamedKey::ArrowRight) => return self.fold_step(true),
                Key::Named(NamedKey::ArrowLeft) => return self.fold_step(false),
                _ => {}
            }
        }
        match key {
            Key::Named(NamedKey::Enter | NamedKey::Space) => {
                if let Some(heading) = self.heading_keyboard() {
                    return self.toggle_heading(&heading);
                }
                if key == &Key::Named(NamedKey::Space) {
                    return false;
                }
                self.focused_key.clone().is_some_and(|key| {
                    let ready = self.model.index_of(&key).is_some_and(|index| {
                        matches!(self.model.row_state(index), Arrival::Ready(()))
                    });
                    if ready {
                        self.activate_key(key);
                    }
                    ready
                })
            }
            Key::Named(NamedKey::F2) if self.on_rename.is_some() => self
                .focused_row_key()
                .is_some_and(|key| self.begin_rename(&key)),
            // The host decides what deleting a row means; the list removes nothing itself.
            Key::Named(NamedKey::Delete) if self.on_delete.is_some() => {
                let Some(key) = self.focused_row_key() else {
                    return false;
                };
                if let Some(callback) = &mut self.on_delete {
                    callback(key);
                }
                true
            }
            _ if self.multi_select && super::rows::is_select_all(key, toggle) => {
                self.selection.select_all();
                self.report_selection(Choosing::ByHand);
                true
            }
            Key::Named(NamedKey::Escape) if self.open.is_some() => self.close_open(),
            Key::Named(NamedKey::Escape)
                if self.multi_select && !self.selection.is_empty(self.model.len()) =>
            {
                self.selection.clear();
                self.report_selection(Choosing::ByHand);
                true
            }
            Key::Character(text)
                if !toggle && !text.is_empty() && text.chars().all(|c| !c.is_control()) =>
            {
                self.type_ahead(text.as_str(), current)
            }
            _ => false,
        }
    }

    fn type_ahead(&mut self, text: &str, current: Option<usize>) -> bool {
        if text == " " && !self.typing.is_typing(crate::time::now()) {
            return false;
        }
        self.typing.type_text(text, crate::time::now());
        let model = &self.model;
        let mut named = false;
        let target = self.typing.target_at(model.len(), current, |index| {
            let label = model.label(index);
            named |= label.is_some();
            label.map(std::borrow::Cow::Owned)
        });
        if !named {
            self.typing.reset();
            return false;
        }
        if let Some(index) = target {
            self.focus_index(index, false);
        }
        true
    }

    fn scroll_by(&mut self, delta: f64) -> bool {
        let before = self.viewport.offset();
        self.set_scroll_offset(before + delta);
        self.viewport.offset().to_bits() != before.to_bits()
    }

    fn handle_scrollbar_event(&mut self, event: &WindowEvent, scale_factor: f64) -> bool {
        if let Some(mut bar) = self.overlay_bar.take() {
            let handled = self.handle_overlay_bar_event(&mut bar, event, scale_factor);
            self.overlay_bar = Some(bar);
            return handled;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = Some(point);
                if self.vbar.is_dragging() {
                    if let Some(track) = self.scrollbar_track() {
                        let metrics = self.scroll_metrics();
                        if let Some(offset) = self.vbar.drag_to(track, metrics, point) {
                            self.set_scroll_offset(offset);
                        }
                    }
                    return true;
                }
                if let Some(track) = self.scrollbar_track() {
                    let metrics = self.scroll_metrics();
                    self.vbar.update_hover(track, metrics, point);
                } else {
                    self.vbar.set_hover(false);
                }
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let (Some(point), Some(track)) = (self.last_cursor, self.scrollbar_track()) else {
                    return false;
                };
                let metrics = self.scroll_metrics();
                match self.vbar.press(track, metrics, point) {
                    PressOutcome::Thumb => true,
                    PressOutcome::Page(offset) => {
                        self.set_scroll_offset(offset);
                        true
                    }
                    PressOutcome::Miss => false,
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.vbar.release(),
            _ => false,
        }
    }

    fn handle_overlay_bar_event(
        &mut self,
        bar: &mut crate::widget::OverlayScrollbar,
        event: &WindowEvent,
        scale_factor: f64,
    ) -> bool {
        let metrics = self.scroll_metrics();
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_cursor = Some(point);
                if bar.is_dragging() {
                    if let Some(offset) = bar.drag_to(self.rect, metrics, point) {
                        self.set_scroll_offset(offset);
                    }
                    return true;
                }
                bar.update_pointer(self.rect, metrics, point);
                false
            }
            WindowEvent::CursorLeft { .. } => {
                bar.pointer_left();
                false
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let Some(point) = self.last_cursor else {
                    return false;
                };
                match bar.press(self.rect, metrics, point) {
                    PressOutcome::Thumb => true,
                    PressOutcome::Page(offset) => {
                        self.set_scroll_offset(offset);
                        true
                    }
                    PressOutcome::Miss => false,
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => bar.release(),
            _ => false,
        }
    }
}

impl<T, K> VirtualList<VecListModel<T, K>>
where
    T: 'static,
    K: Clone + Eq + Hash + 'static,
{
    pub fn from_items(
        items: Vec<T>,
        estimated_row_height: f32,
        key: impl FnMut(&T) -> K,
        mut row: impl FnMut(&T, usize, &K) -> Element + 'static,
    ) -> Result<Self, DuplicateListKey> {
        let model = VecListModel::new(items, key)?;
        Ok(Self::new(
            model,
            estimated_row_height,
            move |model, index, key| {
                model.item(index).map_or_else(
                    || Box::new(crate::widget::Text::new("")) as Element,
                    |item| row(item, index, key),
                )
            },
        ))
    }

    #[must_use]
    pub fn reorderable(self) -> Self {
        self.with_reorder(
            |_, source, target| target.placement != DropPlacement::Inside && source != &target.key,
            |model, dropped| {
                let Some(from) = model.index_of(&dropped.source) else {
                    return false;
                };
                let Some(target) = model.index_of(&dropped.target.key) else {
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
                model.move_item(from, to)
            },
        )
    }
}

impl<M: ListModel> crate::widget::virtual_list::PaintedRowChevrons for VirtualList<M> {
    fn painted_row_chevrons(&self) -> Vec<crate::widget::virtual_list::RowChevron> {
        self.painted_chevrons.clone()
    }
}

impl<M: ListModel> crate::widget::PaintedRowLifts for VirtualList<M> {
    fn painted_row_lifts(&self) -> Vec<crate::widget::RowLift> {
        self.painted_lifts.clone()
    }
}

impl<M: ListModel + 'static> Widget for VirtualList<M> {
    fn html(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        cx.element("ul");
        cx.children();
        let chosen: Vec<vello::kurbo::Rect> = self
            .mounted
            .iter()
            .filter(|row| self.row_selection(row.index).selected)
            .map(|row| row.widget.get_rect())
            .collect();
        if !chosen.is_empty() {
            cx.choose_children(&chosen);
        }
    }

    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::widget::ResourceOperations,
        dyn crate::widget::PaintedRowLifts,
        dyn crate::widget::virtual_list::PaintedRowChevrons
    );

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        if self.interaction == environment {
            return;
        }
        self.interaction = environment;
        self.viewport
            .set_minimum_height(f64::from(environment.minimum_collection_row()));
        self.viewport
            .set_overscan(self.viewport.estimated_height() * 2.0);
        for row in &mut self.mounted {
            row.widget.set_interaction_environment(environment);
        }
        self.visible_window_changed();
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let scrolled_only = !self.structure_dirty
            && !self.request_dirty
            && !self.mounted_covers(&self.rows_to_hold());
        if !scrolled_only {
            self.reconcile_mounted();
        }
        for row in &mut self.mounted {
            row.widget.sync(state, path);
        }
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let now = crate::time::now();
        let elapsed = self
            .last_tick
            .replace(now)
            .map_or(crate::time::Duration::ZERO, |last| {
                now.saturating_duration_since(last)
            });
        let legacy_wakeup = self.model.poll_changed();
        if legacy_wakeup {
            self.notify_model_changed();
        }
        let model_changes = self.model.poll_changes();
        let received_changes = !model_changes.is_empty();
        if received_changes {
            self.notify_model_changes(&model_changes);
        }
        if legacy_wakeup || received_changes {
            self.open_held();
        }
        let mut changed = legacy_wakeup || received_changes;
        changed |= self.take_retry();
        let offset = self.viewport.offset();
        if self.scroll.momentum.is_active() {
            let delta = self.scroll.momentum.advance(now);
            if !self.scroll_by(delta) {
                self.scroll.momentum.stop();
            }
        }
        self.follow_scroll_to(crate::motion::frame_now());
        if self.viewport.offset().to_bits() != offset.to_bits() {
            crate::widget::note_layout_moved();
        }
        if self.reorder.is_active() {
            let motion = self.reorder.tick(self.rect, elapsed, now);
            if let Some(key) = motion.hover_expand {
                changed |= self.unfold_for_drop(&key);
            }
            if motion.scroll_delta != 0.0 && self.scroll_by(motion.scroll_delta) {
                changed = true;
                if let Some(cursor) = self.last_cursor {
                    self.update_reorder_pointer(cursor, now);
                }
            }
        }
        for row in &mut self.mounted {
            changed |= row.widget.update(state, path);
        }
        changed
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.reduce_motion = theme.reduce_motion;
        if self.reduce_motion {
            self.scroll.momentum.stop();
        }
        self.gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        if let Some(bar) = &mut self.overlay_bar {
            bar.set_theme(theme);
        }
        self.bleed = match self.stance {
            RowStance::Flush => 0.0,
            RowStance::Bleed => crate::chrome::row_bleed(theme),
            RowStance::Column => crate::chrome::column_gutter(theme),
        };
        self.lift = crate::chrome::row_bleed(theme);
        self.content_gutter = match self.stance {
            RowStance::Column => crate::chrome::column_gutter(theme),
            RowStance::Bleed | RowStance::Flush => 0.0,
        };
        let disclosure = crate::chrome::disclosure_gutter(theme);
        if (disclosure - self.disclosure).abs() > 0.5 {
            let mounted: Vec<M::Key> = self.mounted.iter().map(|row| row.key.clone()).collect();
            self.invalidate_rows(mounted);
        }
        self.disclosure = disclosure;
        self.row_line = f64::from(theme.tokens.metrics.row_single);
        self.settle_lead();
        self.pill_height =
            crate::widget::ButtonFace::height(crate::widget::ControlSize::Regular, theme);
        self.pill_width = f64::from(
            theme
                .tokens
                .typography
                .size(crate::tokens::TextRole::Caption),
        ) * 24.0
            + f64::from(theme.tokens.spacing.s4) * 6.0;
        self.motion = self.feel.over(theme.motion());
        self.groups.theme(theme, self.interaction);
        if let Some(edges) = self.edges.as_mut() {
            edges.theme(theme);
        }
        self.update_edges();
        if self.keep_end() {
            self.refresh_anchor();
        }
        if self.fit_rows {
            self.viewport.set_viewport(self.viewport.content_extent());
        }
        self.reconcile_mounted();
        self.layout_offset = self.viewport.offset();
        let mut child_nodes = Vec::with_capacity(self.mounted.len());
        for row in &mut self.mounted {
            let child = row.widget.build_layout(taffy, theme, node_map);
            row.node = Some(child);
            row.replacing = None;
            child_nodes.push(child);
        }
        self.place_rows(taffy);
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: if self.fill_height {
                        percent(1.0_f32)
                    } else if self.fit_rows {
                        Dimension::length(px(self.viewport.content_extent()))
                    } else {
                        Dimension::length(px(self.viewport.viewport()))
                    },
                },
                overflow: taffy::Point {
                    x: taffy::Overflow::Clip,
                    y: taffy::Overflow::Clip,
                },
                ..Default::default()
            },
            &child_nodes,
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.structure_dirty = false;
        self.remount = false;
        self.letting_go.clear();
        self.layout_node = Some(node);
        node
    }

    fn remount_children(&mut self, remount: &mut crate::widget::Remount<'_>) -> bool {
        if self.remount || self.structure_dirty {
            return self.remount_rows(remount);
        }
        self.mounted
            .iter_mut()
            .all(|row| row.widget.remount_children(remount))
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if !self.in_place {
            self.set_open(None);
        }
        if self.fill_height
            && self.rect.height() > 0.0
            && (self.rect.height() - self.viewport.viewport()).abs() > 0.5
        {
            self.viewport.set_viewport(self.rect.height());
            self.keep_end();
            self.visible_window_changed();
        }
        let slack = self.end_slack();
        let content = crate::widget::layout::snap_to_device_pixels(
            slack + self.layout_offset - self.viewport.offset(),
        );
        let child_origin = Point::new(self.rect.x0, self.rect.y0 + content);
        let near = self.mounted_near();
        let again = crate::widget::placing_again(taffy);
        let before = std::mem::replace(&mut self.near_placed, near.clone());
        let placing: Vec<usize> = if again {
            near.clone()
                .chain(before.filter(|index| !near.contains(index)))
                .collect()
        } else {
            (0..self.mounted.len()).collect()
        };
        self.placed = (again && !near.is_empty()).then_some(near);
        let mut measurements = Vec::with_capacity(placing.len());
        for at in placing {
            let Some(row) = self.mounted.get_mut(at) else {
                continue;
            };
            let Some(child) = row.node else {
                continue;
            };
            row.widget.apply_layout(taffy, child, child_origin);
            if !again {
                measurements.push((row.index, row.key.clone(), row.widget.get_rect().height()));
            }
        }
        // A row measured anew moves the rows after it: a rebuild no damage describes.
        let mut remeasured = self.update_edges();
        let mut first_measured = false;
        for (index, key, height) in measurements {
            let height = self.grouped_height(index, height);
            let geometry = |viewport: &SparseVirtualViewport<M::Key>| {
                (
                    viewport.item_height(index).map(f64::to_bits),
                    viewport.offset().to_bits(),
                    viewport.content_extent().to_bits(),
                )
            };
            let first = !self.viewport.is_measured(&key);
            let before = geometry(&self.viewport);
            let shown = self.grown_height(index, &key, height.max(1.0), first);
            self.viewport.measure(index, key, shown);
            let moved = geometry(&self.viewport) != before;
            if first {
                first_measured |= moved;
            } else {
                remeasured |= moved;
            }
        }
        remeasured |= self.keep_end();
        remeasured |= (self.end_slack() - slack).abs() > 0.5;
        let extent = self.viewport.content_extent();
        if self.fit_rows && fit_needs_layout(self.rect.height(), extent, self.viewport.viewport()) {
            self.viewport.set_viewport(extent);
            self.box_dirty = true;
            remeasured = true;
        }
        if remeasured {
            self.visible_window_changed();
        } else if first_measured {
            self.remount = true;
            self.layout_dirty = true;
            self.refresh_anchor();
        } else {
            self.layout_dirty = false;
            self.refresh_anchor();
        }
        if !self.mounted_covers(&self.rows_to_hold()) {
            self.remount = true;
        }
        self.scroll_dirty = false;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.paint(scene, transform, text_renderer, theme, presence);
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keeps child, scrollbar, reorder, scroll, selection, and keyboard precedence explicit"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.scroll.momentum.set_device_scale(ctx.scale_factor);
        self.scroll.device_scale = ctx.scale_factor;
        if self.route_menu(event, ctx) {
            return;
        }
        if matches!(
            event,
            WindowEvent::MouseWheel { .. }
                | WindowEvent::KeyboardInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
        ) && self
            .scroll
            .to
            .take_over(crate::motion::frame_now())
            .is_some()
        {
            self.refresh_anchor();
        }
        if self.handle_scrollbar_event(event, ctx.scale_factor) {
            *ctx.handled = true;
            return;
        }
        if self.route_rename(event, ctx) {
            return;
        }
        if let Some(cursor) = self.last_cursor
            && match event {
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    button: MouseButton::Left,
                    ..
                } => self.hold_band(cursor),
                WindowEvent::MouseInput {
                    state: ElementState::Released,
                    button: MouseButton::Left,
                    ..
                } => self.release_band(cursor),
                _ => false,
            }
        {
            if matches!(
                event,
                WindowEvent::MouseInput {
                    state: ElementState::Pressed,
                    ..
                }
            ) {
                *ctx.focus_target = Some(ctx.node_id);
            }
            *ctx.handled = true;
            return;
        }
        let taking_press = self.takes_row_press(event);
        let pressed = match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let inside = self
                    .last_cursor
                    .is_some_and(|point| self.rect.contains(point));
                self.pressed_row = if inside { self.row_at_cursor() } else { None };
                None
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.pressed_row.take(),
            _ => None,
        };
        if !taking_press
            && !self.drag_scroll.is_tracking()
            && !self.reorder.is_tracking()
            && self.pointer_reaches_rows(event, ctx)
        {
            let reached = self.rows_reached(event);
            if let Some(rows) = self.mounted.get_mut(reached) {
                crate::widget::route_to_mounted(
                    rows,
                    |mounted| (&mut mounted.widget, mounted.node),
                    event,
                    ctx,
                );
            }
        }
        if *ctx.handled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                self.jump_hovered = self.pill_rect().is_some_and(|rect| rect.contains(point));
                if self.reorder.is_tracking() {
                    self.scroll.momentum.stop();
                    self.update_reorder_pointer(point, crate::time::now());
                    self.last_tick = Some(crate::time::now());
                    *ctx.handled = self.reorder.is_active();
                } else if self.drag_scroll.is_tracking() {
                    let now = crate::time::now();
                    let update = self.drag_scroll.update(point.y, now);
                    self.scroll.momentum.stop();
                    if let Some(update) = update {
                        self.set_scroll_offset(update.target_offset);
                        self.last_tick = Some(now);
                        *ctx.handled = true;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self
                .last_cursor
                .is_some_and(|point| self.rect.contains(point)) =>
            {
                let cursor = self.last_cursor.unwrap_or_default();
                if self.pill_rect().is_some_and(|rect| rect.contains(cursor)) {
                    self.jump_pressed = true;
                    *ctx.handled = true;
                    return;
                }
                if self.reorder_validator.is_some()
                    && self.reorder_commit.is_some()
                    && let Some((_, key)) = self.row_at_cursor()
                {
                    self.reorder.begin(key, cursor);
                    self.last_tick = Some(crate::time::now());
                } else {
                    self.drag_scroll
                        .begin(cursor.y, self.viewport.offset(), crate::time::now());
                }
                self.scroll.momentum.stop();
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if std::mem::take(&mut self.jump_pressed) {
                    if self.last_cursor.is_some_and(|point| {
                        self.pill_rect().is_some_and(|rect| rect.contains(point))
                    }) {
                        self.follow_pill();
                    }
                    *ctx.handled = true;
                    return;
                }
                if self.reorder.is_tracking() {
                    let source = self.reorder.source().cloned();
                    let active = self.reorder.is_active();
                    if let Some(dropped) = self.reorder.finish() {
                        self.commit_reorder(dropped);
                    } else if !active && let Some(key) = source {
                        self.press_row(key, *ctx.modifiers);
                    }
                    *ctx.handled = true;
                } else if let Some(drag) = self.drag_scroll.finish(crate::time::now()) {
                    if drag.dragged {
                        if self.reduce_motion {
                            self.scroll.momentum.stop();
                        } else {
                            self.scroll.momentum.fling(drag.velocity);
                            self.last_tick = Some(crate::time::now());
                        }
                    } else if let Some((_, key)) = self.row_at_cursor() {
                        self.press_row(key, *ctx.modifiers);
                    } else {
                        self.close_open();
                    }
                    *ctx.handled = true;
                } else if let Some((_, key)) = pressed
                    && self.row_at_cursor().is_some_and(|(_, at)| at == key)
                {
                    self.press_row(key, *ctx.modifiers);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.menus.offers_row_menu() => {
                if let (Some((_, key)), Some(point)) = (self.row_at_cursor(), self.last_cursor) {
                    self.open_row_menu(key, point);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                } else if let Some(point) =
                    self.last_cursor.filter(|point| self.rect.contains(*point))
                    && self.open_context_menu(None, point)
                {
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, phase, .. }
                if self
                    .last_cursor
                    .is_some_and(|point| self.rect.contains(point))
                    && !crate::content_zoom::is_zoom_scroll(event, ctx.modifiers) =>
            {
                self.last_tick = Some(crate::time::now());
                *ctx.handled = match self
                    .scroll
                    .momentum
                    .wheel(delta, *phase, self.reduce_motion)
                {
                    WheelStep::Scroll(delta) => {
                        let before = self.viewport.offset();
                        self.scroll_by(delta);
                        let moved = self.viewport.offset() - before;
                        self.scroll.momentum.wheel_applied(delta, moved)
                    }
                    WheelStep::Fling => true,
                    WheelStep::Ignored => false,
                };
                if *ctx.handled {
                    crate::widget::note_view_only_pan();
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if (self.is_focused || self.mounted.iter().any(|row| row.widget.is_focused()))
                    && event.state == ElementState::Pressed =>
            {
                self.scroll.momentum.stop();
                if event.logical_key == Key::Named(NamedKey::Escape) && self.reorder.cancel() {
                    *ctx.handled = true;
                } else if self.take_menu_key(&event.logical_key, ctx.modifiers.state()) {
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                } else if let Some(movement) =
                    self.tree_move_chord(&event.logical_key, ctx.modifiers.state())
                {
                    let moved = self.move_focused_step(movement);
                    *ctx.handled = moved;
                    if moved
                        && self.is_focused
                        && let Some(node) = self.focused_row_node()
                    {
                        *ctx.focus_target = Some(node);
                    }
                } else if let Some(up) =
                    self.reorder_chord(&event.logical_key, ctx.modifiers.state())
                {
                    let moved = if self.tree.is_some() {
                        self.move_focused_step(if up { TreeMove::Up } else { TreeMove::Down })
                    } else {
                        self.move_focused(up)
                    };
                    *ctx.handled = true;
                    if moved
                        && self.is_focused
                        && let Some(node) = self.focused_row_node()
                    {
                        *ctx.focus_target = Some(node);
                    }
                } else {
                    let toggle =
                        ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key();
                    *ctx.handled = self.on_key(
                        &event.logical_key,
                        toggle,
                        ctx.modifiers.state().shift_key(),
                    );
                    if *ctx.handled && !self.is_focused {
                        *ctx.focus_target = Some(ctx.node_id);
                    }
                }
            }
            _ => {}
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.mounted.iter().map(|row| row.widget.as_ref()).collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for row in &self.mounted {
            visitor(row.widget.as_ref());
        }
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        if self.letting_go.is_empty() {
            for row in &self.mounted {
                visitor(row.laid_out_widget());
            }
            return true;
        }
        let mut rows = self
            .mounted
            .iter()
            .map(|row| (row.index, row.laid_out_widget()))
            .chain(
                self.letting_go
                    .iter()
                    .map(|(place, row)| (*place, row.widget.as_ref() as &dyn Widget)),
            )
            .collect::<Vec<_>>();
        rows.sort_by_key(|(place, _)| *place);
        for (_, widget) in rows {
            visitor(widget);
        }
        true
    }

    fn for_each_child_in_clip(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for row in self.mounted.get(self.placed_near()).unwrap_or_default() {
            visitor(row.widget.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.mounted
            .iter_mut()
            .map(|row| row.widget.as_mut() as &mut dyn Widget)
            .collect()
    }

    fn accessibility_represents_child(&self, child_index: usize) -> bool {
        child_index < self.mounted.len()
    }

    fn accessibility_child_path(&self, child_index: usize) -> Option<Vec<usize>> {
        let row = self.mounted.get(child_index)?;
        self.handing_controls(row)
            .then(|| vec![row.index.saturating_add(self.headings_before(child_index))])
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [child] = path else {
            return false;
        };
        let Some(index) = (0..self.mounted.len())
            .find(|index| index.saturating_add(self.headings_before(*index)) == *child)
        else {
            return false;
        };
        let Some(key) = self.mounted.get(index).map(|row| row.key.clone()) else {
            return false;
        };
        self.focus_row(key.clone());
        self.select_focused(key.clone(), false);
        if let Some(at) = self.model.index_of(&key) {
            self.scroll_to_index(at);
        }
        true
    }

    fn name(&self) -> &'static str {
        "VirtualList"
    }

    fn test_id(&self) -> Option<&str> {
        self.test_id.as_deref()
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        // Focus leaving the list ends an inline rename, keeping the edit.
        if !focused && self.rename.is_some() {
            self.commit_rename();
        }
        self.is_focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn context_menu_anchor(&self) -> Option<Rect> {
        let focus_inside = self.is_focused
            || self
                .mounted
                .iter()
                .any(|row| crate::widget::context_menu_area::focus_within(row.widget.as_ref()));
        if !focus_inside {
            return None;
        }
        let key = self.focused_row_key()?;
        self.row_box(self.model.index_of(&key)?)
    }

    fn touch_claim(&self, point: vello::kurbo::Point) -> crate::interaction::TouchClaim {
        self.mounted
            .iter()
            .find(|row| row.widget.get_rect().contains(point))
            .map_or(crate::interaction::TouchClaim::Pointer, |row| {
                row.widget.touch_claim(point)
            })
    }

    fn wants_pointer_capture(&self) -> bool {
        self.drag_scroll.is_tracking()
            || self.reorder.is_tracking()
            || self.vbar.is_dragging()
            || self
                .overlay_bar
                .as_ref()
                .is_some_and(crate::widget::OverlayScrollbar::is_dragging)
            || self
                .mounted
                .iter()
                .any(|row| row.widget.wants_pointer_capture())
    }

    fn needs_animation_frame(&self) -> bool {
        let now = crate::motion::frame_now();
        self.overlay_bar
            .as_ref()
            .is_some_and(crate::widget::OverlayScrollbar::needs_animation_frame)
            || self.scroll.momentum.is_active()
            || self
                .scroll
                .momentum
                .give_needs_frame(crate::motion::frame_now())
            || self.reveal_in_progress(crate::motion::frame_now())
            || self.growth.iter().any(|(_, growth)| growth.is_moving(now))
            || self.row_motion.is_moving(now)
            || self.arrivals.is_moving(now)
            || self.reorder.is_active()
            || self.model.needs_poll()
            || self.rename_animating()
            || self
                .mounted
                .iter()
                .any(|row| row.widget.needs_animation_frame())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty)
            | std::mem::take(&mut self.box_dirty)
            | std::mem::take(&mut self.remount);
        for row in &mut self.mounted {
            dirty |= row.widget.take_structure_dirty();
        }
        dirty
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        use crate::widget::StructureChange;
        let mut change = if std::mem::take(&mut self.box_dirty) {
            StructureChange::Rebuild
        } else if (self.structure_dirty || self.remount) && self.layout_node.is_some() {
            StructureChange::Children
        } else {
            StructureChange::None
        };
        for row in &mut self.mounted {
            change = change.join(row.widget.take_structure_change());
        }
        change
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        let in_motion = self.shown.drawing().is_some_and(|shown| shown.in_motion);
        let shown_changed = match (self.shown.drawing(), self.shown.presented()) {
            (Some(drawing), Some(presented)) => *drawing != *presented,
            (drawing, presented) => drawing.is_some() != presented.is_some(),
        };
        if in_motion {
            regions.push(self.rect);
            return;
        }
        let now = crate::motion::frame_now();
        let own_redraw =
            self.scroll.momentum.give_needs_frame(now) || self.arrivals.needs_redraw(now);
        match self.shown.compare(self, regions) {
            crate::scroll_copy::Since::Copied => self.collect_row_damage(regions),
            crate::scroll_copy::Since::Still if !shown_changed && !own_redraw => {
                self.collect_row_damage(regions);
            }
            crate::scroll_copy::Since::Still | crate::scroll_copy::Since::Repaint => {
                if shown_changed || (self.needs_redraw() || self.needs_animation_frame()) {
                    regions.push(self.rect);
                }
            }
        }
    }

    fn needs_redraw(&self) -> bool {
        self.scroll
            .momentum
            .give_needs_frame(crate::motion::frame_now())
            || self.arrivals.needs_redraw(crate::motion::frame_now())
            || self.rename_needs_redraw()
            || self.menu_needs_redraw()
            || super::defaults::needs_redraw(&|visit| self.for_each_child(visit))
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        super::defaults::draw_overlay(
            self.children_mut(),
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
        self.paint_menu(scene, transform, text_renderer, theme, presence);
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.menu_capture()
    }

    fn traps_focus(&self) -> bool {
        self.context_menu_is_open()
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let bar = self
            .overlay_bar
            .as_ref()
            .and_then(crate::widget::OverlayScrollbar::next_wake_deadline);
        match (self.arrivals_wake(), bar) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn mark_drawn(&mut self) {
        self.shown.present();
        if self.rename.is_some() {
            self.mark_rename_drawn();
        }
        if let Some(menu) = self.menus.owned_mut() {
            menu.menu_drawn();
        }
        for row in &mut self.mounted {
            row.widget.mark_drawn();
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty
            || self.scroll_dirty
            || !self.growth.is_empty()
            || self.mounted.iter().any(|row| row.widget.is_layout_dirty())
    }

    fn layout_needs_measure(&self) -> bool {
        self.layout_dirty
            || !self.growth.is_empty()
            || self
                .mounted
                .iter()
                .any(|row| row.widget.layout_needs_measure())
    }

    fn layout_info(&self) -> Option<crate::widget::LayoutInfo> {
        Some(crate::widget::LayoutInfo::List(crate::widget::ListLayout {
            heads_rows: groups::group_of(&self.model, 0, self.tail_title.as_deref()).is_some(),
            lead: crate::num::f32_of_f64(self.viewport.edges().0),
        }))
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let enabled = self.is_enabled();
        Some(
            crate::a11y::SharedAccessNode {
                node: self.access_root(enabled),
                children: self.accessibility_children(enabled),
            }
            .into_owned(),
        )
    }

    fn accessibility_shared(&self) -> Option<crate::a11y::SharedAccessNode> {
        let enabled = self.is_enabled();
        Some(crate::a11y::SharedAccessNode {
            node: self.access_root(enabled),
            children: self.accessibility_children(enabled),
        })
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        self.perform_a11y_action(target, action, state, path)
    }
}

#[path = "virtual_list_a11y.rs"]
mod a11y;

#[cfg(test)]
#[path = "virtual_list_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "virtual_list_group_tests.rs"]
mod group_tests;

#[cfg(test)]
#[path = "virtual_list_copy_tests.rs"]
mod copy_tests;

#[path = "virtual_list_arrivals.rs"]
mod arrivals;

#[cfg(test)]
#[path = "virtual_list_arriving_tests.rs"]
mod arriving_tests;

#[cfg(test)]
#[path = "virtual_list_mount_tests.rs"]
mod mount_tests;

#[cfg(test)]
#[path = "virtual_list_tree_tests.rs"]
mod tree_tests;

#[cfg(test)]
#[path = "virtual_list_open_tests.rs"]
mod open_tests;

#[path = "virtual_list_choosing.rs"]
mod choosing;

#[path = "virtual_list_open.rs"]
mod open;
#[path = "virtual_list_paint.rs"]
mod paint;

#[path = "virtual_list_component.rs"]
mod component;
pub(crate) use component::by_hand;
pub use component::{DescribedList, DescribedRows, ItemRow, ListItemProps, VirtualListProps};
