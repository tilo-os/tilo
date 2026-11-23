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
#[path = "virtual_list_typed.rs"]
mod typed;
pub use typed::{AnyModel, ListKey, VirtualList};

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
#[path = "virtual_list_html_tree.rs"]
mod html_tree;
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
/// The tallest a list that fills its parent says its rows come to where the parent has no height
/// to give ([`VirtualList::natural_style`]), in logical pixels: a tall screen's height. Past it the
/// list scrolls in whatever room it is given, and a list never says a height that would have every
/// one of thousands of rows mounted.
const NATURAL_MOST: f64 = crate::tokens::Layout::BASE.natural_most as f64;
const END_SLACK: f64 = 0.5;
/// How much of the last screen counts as the end of a list, in viewport heights: the person is
/// read to its end while a page that would follow it is still worth asking for.
const END_REACH_SCREENS: f64 = 1.0;
/// How far from the end the person reads away before the list says the end again, in viewport
/// heights: more than the reach that told it, so a step around the boundary tells once.
const END_REARM_SCREENS: f64 = 2.0;

/// What a list as tall as its rows shows of them, in its own coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
enum FitShown {
    /// Not laid out yet: the rows of a tall screen from its top.
    Unplaced,
    /// Nothing clips it: every row shows.
    Whole,
    /// The band of it the scroll view around it shows.
    Band(f64, f64),
}

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

type RowBuilder = Box<dyn FnMut(&dyn AnyModel, usize, &ListKey, RowBuild) -> Element>;
type KeyCallback<K> = Box<dyn FnMut(K)>;
type FoldCallback<K> = Box<dyn FnMut(&K, bool)>;
type SemanticKey<K> = Box<dyn Fn(&K) -> String>;
type RowPresence =
    Box<dyn Fn(&dyn AnyModel, usize, &ListKey) -> Option<crate::presence::PresenceKey>>;
type SelectCallback<K> = Box<dyn FnMut(K, bool)>;
type SelectedCallback<K> = Box<dyn FnMut(&[K])>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Choosing {
    ByHand,
    Following,
}
type ReorderValidator = Box<dyn FnMut(&dyn AnyModel, &ListKey, &DropTarget<ListKey>) -> bool>;
type ReorderCommit = Box<dyn FnMut(&mut dyn AnyModel, ReorderDrop<ListKey>) -> bool>;
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

/// Where a list stands its rows, and who keeps the edge they start on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RowStance {
    /// A list inside a page: the page keeps the edge, so the rows' words start on the list's own
    /// edge, where the words over and under it start, and a row's lift hangs a bleed past it,
    /// into the page's margin.
    Bleed,
    /// Rows edge to edge, lifted inside their box.
    Flush,
    /// A column's whole body: the list keeps the column's gutter itself, at its sides and over
    /// its first row.
    Column,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "focus, drag threshold, fetch, structural, and layout invalidation are independent widget states"
)]
#[crate::component::not_a_component(
    part,
    "the rows of a `VirtualList`, which reads them as its model's"
)]
pub struct VirtualRows {
    test_id: Option<String>,
    model: Box<dyn AnyModel>,
    row_builder: RowBuilder,
    selection_aware: bool,
    row_presence: Option<RowPresence>,
    tooltips_suppressed: bool,
    delegate_selection: bool,
    viewport: SparseVirtualViewport<ListKey>,
    mounted: Vec<MountedRow<ListKey>>,
    /// Rows let go of whose nodes the retained layout still holds: they keep its walk's
    /// place, and their widgets alive, until a remount unmounts them.
    letting_go: Vec<(usize, LaidOutRow)>,
    selection: KeySelection<ListKey>,
    multi_select: bool,
    reading: bool,
    follow_focus: bool,
    focused_key: Option<ListKey>,
    /// The group's heading holding the keyboard, with the index of the first row under it.
    focused_group: Option<(String, usize)>,
    typing: crate::type_ahead::TypeAhead,
    on_select: Option<SelectCallback<ListKey>>,
    on_selected: Option<SelectedCallback<ListKey>>,
    reported_many: bool,
    view: Option<KeyCallback<ListKey>>,
    on_activate: Option<KeyCallback<ListKey>>,
    open_on_press: bool,
    on_fold: Option<FoldCallback<ListKey>>,
    on_opened: Option<KeyCallback<ListKey>>,
    /// The row a view asked the list to show, once it has moved to it: the view takes back the
    /// ask on it, so the same row can be asked for again after a person scrolled away.
    on_revealed: Option<KeyCallback<ListKey>>,
    on_closed: Option<Box<dyn FnMut()>>,
    /// The person read the list to its end: told once each time they arrive there, and not
    /// again until they read away and come back, so a list that shows one page of a longer
    /// thing asks for the next page once per arrival.
    on_end_reached: Option<Box<dyn FnMut()>>,
    /// The end was told and the person has not read away from it since.
    end_told: bool,
    open: Option<ListKey>,
    rereading_open: Option<ListKey>,
    held_open: Option<String>,
    growth: Vec<(ListKey, crate::motion::MotionValue<f64>)>,
    growing: HashSet<ListKey>,
    menus: RowMenus,
    clicks: super::rows::Clicks<ListKey>,
    semantic_key: Option<SemanticKey<ListKey>>,
    accessible_name: Option<String>,
    rect: Rect,
    last_cursor: Option<Point>,
    is_focused: bool,
    drag_scroll: DragScroller,
    pressed_row: Option<(usize, ListKey)>,
    /// The row press this list took, carried to an equivalent list built in its place before the
    /// release ([`crate::pointer_replay`]).
    held_row: crate::pointer_replay::HeldPress,
    reorder: ReorderController<ListKey>,
    reorder_validator: Option<ReorderValidator>,
    reorder_commit: Option<ReorderCommit>,
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
    /// What a list that fills its parent's height says its rows come to where that parent has no
    /// height of its own to give (a place on a canvas that hugs what it holds): an empty node in
    /// the layout as tall as the rows, up to [`NATURAL_MOST`]. Under a parent with a height the
    /// list takes that height and the node is only inside it.
    natural: Option<NodeId>,
    fit_rows: bool,
    /// What of a list as tall as its rows ([`Self::fitting_rows`]) the scroll view around it
    /// shows, as its last layout found.
    fit_shown: FitShown,
    follow_end: bool,
    at_end: bool,
    last_tick: Option<Instant>,
    last_anchor: Option<SparseAnchor<ListKey>>,
    requested_range: Option<Range<usize>>,
    requested_open: Option<ListKey>,
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
    invalidated: HashSet<ListKey>,
    row_builds: u64,
    access_rows: std::cell::RefCell<a11y::KeptAccessRows<ListKey>>,
    layout_node: Option<NodeId>,
    shown: crate::scroll_copy::ShownFrames<ListShown>,
    row_motion: crate::collection_transition::RowMotions<ListKey>,
    arrivals: ArrivingRows<ListKey>,
    retry_asked: Rc<Cell<Option<usize>>>,
    placed: Option<Range<usize>>,
    near_placed: Range<usize>,
    unread: Option<String>,
    jump_hovered: bool,
    jump_pressed: bool,
    /// Whether the pill is the control the keyboard or a reader is on, rather than a row: the way
    /// back is pressed with Return or Space, as it is with a pointer.
    jump_focused: bool,
    pill_height: f64,
    /// The pill's width for each way back ([`crate::widget::conversation::Jump::slot`]): the words
    /// measured at the face they are drawn in, so a narrow list shows them whole.
    pill_widths: [f64; crate::widget::conversation::JUMPS],
    names_item: std::cell::RefCell<Option<Box<dyn Fn(&str) -> Option<ListKey>>>>,
    groups: Groups,
    tail_title: Option<String>,
    view_title: Option<String>,
    view_count: Option<usize>,
    heads_nothing: std::cell::Cell<Option<bool>>,
    row_groups: HashMap<ListKey, Option<groups::Group>>,
    tree: Option<Rc<tree::Marks>>,
    tree_rows: HashMap<ListKey, TreeRow<ListKey>>,
    tree_hidden: HashSet<ListKey>,
    first_level_branch: Option<bool>,
    column_rows: HashSet<ListKey>,
    rename: Option<Box<RenameField<ListKey>>>,
    on_rename: Option<RenameCallback<ListKey>>,
    rename_on_double_click: bool,
    tab_indents: bool,
    on_delete: Option<KeyCallback<ListKey>>,
    /// The row a view asks the list to show, read at every sync: what a press in the view
    /// made of the rows.
    reveal_asked: Option<Box<dyn FnMut() -> Option<ListKey>>>,
    /// The row last asked for that the model does not hold yet: shown as soon as it does.
    reveal_pending: Option<ListKey>,
    /// The item a view asked the list to show, by the id its model presents it as: resolved to a
    /// row as the rows are there, so an ask made before they arrived is still shown.
    reveal_item: Option<String>,
    /// The next reveal scrolls the row into view without choosing it: what a view asks when it
    /// shows the list a row of its own (the first card under a letter), not a row a press made.
    reveal_without_choosing: bool,
}

impl VirtualRows {
    /// Take what the model says changed since it was last asked, and follow it. Whether
    /// anything did.
    fn take_model_changes(&mut self) -> bool {
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
        legacy_wakeup || received_changes
    }

    pub(super) fn build(
        model: Box<dyn AnyModel>,
        estimated_row_height: f32,
        row_builder: RowBuilder,
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
            on_activate: None,
            open_on_press: false,
            on_fold: None,
            on_opened: None,
            on_revealed: None,
            on_closed: None,
            on_end_reached: None,
            end_told: false,
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
            held_row: crate::pointer_replay::HeldPress::default(),
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
            lead: 0.0,
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
            natural: None,
            fit_rows: false,
            fit_shown: FitShown::Unplaced,
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
            jump_focused: false,
            reveal_asked: None,
            reveal_pending: None,
            reveal_item: None,
            reveal_without_choosing: false,
            pill_height: 0.0,
            pill_widths: [0.0; crate::widget::conversation::JUMPS],
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

    pub fn reveal(&mut self, key: &ListKey) -> bool {
        let shown = self.scroll_to_key(key);
        if shown {
            self.focus_row(key.clone());
        }
        shown
    }

    /// Scroll `key`'s row into view, leaving the keyboard where it is; whether the model
    /// holds the row.
    pub(super) fn scroll_to_key(&mut self, key: &ListKey) -> bool {
        let Some(index) = self.model.index_of(key) else {
            return false;
        };
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

    /// Show the row `asked` names whenever it names one, without taking the keyboard:
    /// what a view asks when a press made an item the list should show (the row scrolls
    /// into view and stands chosen). A row the model does not hold yet is shown as soon
    /// as it does.
    #[must_use]
    pub fn following_reveal(
        mut self,
        asked: Box<dyn FnMut() -> Option<ListKey>>,
        initial: Option<ListKey>,
    ) -> Self {
        self.reveal_asked = Some(asked);
        self.reveal_pending = initial;
        self
    }

    /// Take the row the view asked to show, and show it; whether it did.
    fn take_reveal_asked(&mut self) -> bool {
        // What a view asked for by id, resolved against the rows as they are now: a row whose
        // page is still arriving is shown when it comes.
        if let Some(item) = self.reveal_item.clone() {
            match self.key_of_item(&item) {
                Some(key) => {
                    self.reveal_item = None;
                    self.reveal_pending = Some(key);
                }
                None if self.model.len() > 0 && self.model.key_at(0).is_none() => return false,
                None => self.reveal_item = None,
            }
        }
        if let Some(asked) = self.reveal_asked.as_mut()
            && let Some(key) = asked()
        {
            self.reveal_pending = Some(key);
        }
        let Some(key) = self.reveal_pending.clone() else {
            return false;
        };
        if self.model.index_of(&key).is_none() {
            return false;
        }
        self.reveal_pending = None;
        let choosing = !std::mem::take(&mut self.reveal_without_choosing);
        if choosing {
            self.choose_quietly(key.clone());
        }
        let moved = self.scroll_to_key(&key);
        if moved && let Some(told) = self.on_revealed.as_mut() {
            told(key);
        }
        moved
    }

    /// The row of the item `item` names: as its model presents it, or as the list's own naming
    /// does (a view's items, whose keys are their ids).
    pub(super) fn key_of_item(&self, item: &str) -> Option<ListKey> {
        self.model.key_presenting(item).or_else(|| {
            self.names_item
                .borrow()
                .as_ref()
                .and_then(|names| names(item))
        })
    }

    /// Show the row of the item `item` names, as its model presents it: it scrolls into view at
    /// the list's motion, without taking the keyboard or the selection. An item the model does
    /// not hold yet is shown as soon as it does, so a row that arrives with its page is still
    /// brought up.
    pub fn scroll_reveal(&mut self, item: impl Into<String>) {
        self.reveal_item = Some(item.into());
        self.reveal_without_choosing = true;
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

    /// Everything the list draws: its box, and the bleed a row's lift hangs past either side of
    /// it where the page around the list keeps the edge ([`RowStance::Bleed`]).
    pub(super) fn drawn(&self) -> Rect {
        let hang = match self.stance {
            RowStance::Bleed => self.bleed,
            RowStance::Flush | RowStance::Column => 0.0,
        };
        self.rect.inflate(hang, 0.0)
    }

    fn trailing_inset(&self) -> f64 {
        let bar = if self.rows_beside_bar {
            self.gutter
        } else {
            0.0
        };
        match self.stance {
            RowStance::Column => self.bleed.max(bar + self.lift),
            RowStance::Bleed | RowStance::Flush => bar,
        }
    }

    /// The empty node a list that fills its parent stands in the layout ([`Self::natural`]): no
    /// width, and as tall as its rows up to [`NATURAL_MOST`]. The list's own height is its
    /// parent's whenever the parent has one; where it has none (a place hugging what it holds is
    /// measured with no height to fill), the list is as tall as this, so the place hugs its rows.
    pub(super) fn natural_style(&self) -> taffy::Style {
        let rows = self.viewport.content_extent() + self.content_gutter;
        taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::length(0.0),
                height: taffy::Dimension::length(px(rows.clamp(0.0, NATURAL_MOST))),
            },
            flex_shrink: 0.0,
            ..taffy::Style::default()
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

    pub fn set_on_selected(&mut self, callback: impl FnMut(&[ListKey]) + 'static) {
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

    pub fn set_selection(&mut self, mut selection: KeySelection<ListKey>) {
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
        presence: impl Fn(&dyn AnyModel, usize, &ListKey) -> Option<crate::presence::PresenceKey>
        + 'static,
    ) -> Self {
        self.row_presence = Some(Box::new(presence));
        self
    }

    pub fn set_on_select(&mut self, callback: impl FnMut(ListKey, bool) + 'static) {
        self.on_select = Some(Box::new(callback));
    }

    pub fn set_on_activate(&mut self, callback: impl FnMut(ListKey) + 'static) {
        self.on_activate = Some(Box::new(callback));
    }

    pub fn set_on_opened(&mut self, callback: impl FnMut(ListKey) + 'static) {
        self.on_opened = Some(Box::new(callback));
    }

    /// Tell the view the row it asked to show has been brought up: it takes the ask back on it,
    /// so the same row can be asked for again after a person scrolled away.
    pub fn set_on_revealed(&mut self, callback: impl FnMut(ListKey) + 'static) {
        self.on_revealed = Some(Box::new(callback));
    }

    #[must_use]
    pub fn on_closed(mut self, callback: impl FnMut() + 'static) -> Self {
        self.set_on_closed(callback);
        self
    }

    pub fn set_on_closed(&mut self, callback: impl FnMut() + 'static) {
        self.on_closed = Some(Box::new(callback));
    }

    /// Tell the view the person read the list to its end: the last rows of a list that shows one
    /// page of a longer thing came into view, and the page after them is the view's to ask for.
    /// It is told once per arrival, not again while the person stays at the end.
    pub fn set_on_end_reached(&mut self, callback: Option<Box<dyn FnMut()>>) {
        self.on_end_reached = callback;
    }

    #[must_use]
    pub fn on_fold(mut self, callback: impl FnMut(&ListKey, bool) + 'static) -> Self {
        self.on_fold = Some(Box::new(callback));
        self
    }

    /// Encodes a key as an opaque, stable string for accessibility and headless row
    /// actions; it must not change while the key names the same item.
    #[must_use]
    pub fn with_semantic_key(mut self, encode: impl Fn(&ListKey) -> String + 'static) -> Self {
        self.semantic_key = Some(Box::new(encode));
        self
    }

    fn key_for_name(&self, encoded: &str) -> Option<ListKey> {
        let encode = self.semantic_key.as_ref()?;
        (0..self.model.len())
            .filter_map(|index| self.model.key_at(index))
            .find(|key| encode(key) == encoded)
    }

    #[must_use]
    pub fn with_reorder(
        mut self,
        validate: impl FnMut(&dyn AnyModel, &ListKey, &DropTarget<ListKey>) -> bool + 'static,
        commit: impl FnMut(&mut dyn AnyModel, ReorderDrop<ListKey>) -> bool + 'static,
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
    pub fn reorder_target(&self) -> Option<&DropTarget<ListKey>> {
        self.reorder.target()
    }

    #[must_use]
    pub fn reorder_preview(&self) -> Option<Rect> {
        self.reorder.preview()
    }

    #[must_use]
    pub fn model(&self) -> &dyn AnyModel {
        &*self.model
    }

    /// Give `key`'s row the keyboard, and bring it into view: a row focused by an action or
    /// another window's step is one the person then types into, so it stands in the window
    /// however far down the list it was.
    pub fn focus_row(&mut self, key: ListKey) {
        self.focused_group = None;
        self.focused_key = Some(key.clone());
        self.jump_focused = false;
        self.scroll_to_key(&key);
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

    /// Tell the view the person read to the end of the list, once per arrival: the last screen
    /// of rows is where a page after them is asked for, and staying there tells nothing more
    /// until they read away by more than that screen and come back. A list that asks for a page
    /// as its end comes into view therefore asks for one page per reading, never once a frame.
    fn note_end_reached(&mut self) {
        if self.on_end_reached.is_none() || self.model.is_empty() {
            return;
        }
        let screen = self.viewport.viewport().max(0.0);
        let remaining = (self.viewport.max_offset() - self.viewport.offset()).max(0.0);
        if remaining > screen * END_REARM_SCREENS {
            self.end_told = false;
            return;
        }
        if self.end_told || remaining > screen * END_REACH_SCREENS {
            return;
        }
        self.end_told = true;
        if let Some(told) = self.on_end_reached.as_mut() {
            told();
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
        self.note_end_reached();
    }

    #[must_use]
    pub fn selection(&self) -> &KeySelection<ListKey> {
        &self.selection
    }

    pub fn selection_mut(&mut self) -> &mut KeySelection<ListKey> {
        &mut self.selection
    }

    #[must_use]
    pub fn focused_key(&self) -> Option<&ListKey> {
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
        let count = self.model.len();
        let near = if self.fit_rows {
            // A list as tall as its rows shows what the scroll view around it shows of it: the
            // rows of a tall screen from its top until its first layout says where it stands.
            let (top, bottom) = match self.fit_shown {
                FitShown::Unplaced => (0.0, crate::virtual_viewport::UNPLACED_VIEWPORT),
                FitShown::Whole => return 0..count,
                FitShown::Band(top, bottom) => (top, bottom),
            };
            let overscan = self.viewport.overscan();
            self.viewport
                .range_between(top - overscan, bottom + overscan)
        } else {
            self.first_rows(self.viewport.visible_range())
        };
        near.start.min(count)..near.end.min(count)
    }

    /// `near`, before any row was measured, stretched to as many rows as fit at the least height
    /// a row has: the rows mounted first cover the view however short they turn out, so measuring
    /// them mounts none after them.
    fn first_rows(&self, near: std::ops::Range<usize>) -> std::ops::Range<usize> {
        let least = self.viewport.minimum_height();
        if self.viewport.resident_measurements() > 0 || least <= 1.0 {
            return near;
        }
        let reach = self.viewport.viewport() + 2.0 * self.viewport.overscan();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a count of rows a view holds, finite and non-negative"
        )]
        let fit = (reach / least).ceil().max(0.0) as usize;
        near.start..near.end.max(near.start.saturating_add(fit))
    }

    pub fn set_scroll_offset(&mut self, offset: f64) {
        self.viewport.set_offset(offset);
        self.note_scrolled();
        self.scroll_dirty = true;
        self.refresh_anchor();
        self.note_end_reached();
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

    fn key_hash(key: &ListKey) -> u64 {
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

    fn row_at_cursor(&self) -> Option<(usize, ListKey)> {
        self.row_at(self.last_cursor?)
    }

    /// What names the row of `key` across a rebuild of the list: the list's id with the row's
    /// stable key. Empty, carrying no press, when the list has no id or its keys no stable name.
    fn carried_row_key(&self, key: &ListKey) -> String {
        match (self.test_id.as_deref(), self.semantic_key.as_ref()) {
            (Some(id), Some(encode)) if !id.is_empty() => format!("{id}\u{1f}{}", encode(key)),
            _ => String::new(),
        }
    }

    /// The row under the pointer as a target its press is held on: its key and where it stands.
    fn row_press_at_cursor(&self) -> Option<(ListKey, String, Rect)> {
        let (index, key) = self.row_at_cursor()?;
        let rect = self.row_box(index)?;
        let carried = self.carried_row_key(&key);
        Some((key, carried, rect))
    }

    pub fn key_at_point(&self, point: Point) -> Option<ListKey> {
        self.row_at(point).map(|(_, key)| key)
    }

    fn row_at(&self, cursor: Point) -> Option<(usize, ListKey)> {
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

    fn reorder_candidate(&mut self, cursor: Point) -> Option<DropCandidate<ListKey>> {
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

    fn select_focused(&mut self, key: ListKey, extend: bool) {
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
        // The pill, while it is the control the keyboard is on: the way back is taken by Return
        // or Space, and any other key goes back to the rows.
        if self.jump_focused {
            self.jump_focused = false;
            match key {
                Key::Named(NamedKey::Enter | NamedKey::Space) => return self.press_pill(),
                Key::Named(NamedKey::Escape) => return true,
                _ => {}
            }
        }
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
        let model = &*self.model;
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

impl crate::widget::virtual_list::PaintedRowChevrons for VirtualRows {
    fn painted_row_chevrons(&self) -> Vec<crate::widget::virtual_list::RowChevron> {
        self.painted_chevrons.clone()
    }
}

impl crate::widget::PaintedRowLifts for VirtualRows {
    fn painted_row_lifts(&self) -> Vec<crate::widget::RowLift> {
        self.painted_lifts.clone()
    }
}

impl Widget for VirtualRows {
    fn html(&self, cx: &mut crate::composition::HtmlCx<'_>) {
        cx.element("ul");
        cx.children();
        // The rows stand in from the list's edges exactly as far as the list laid them out, so a
        // press lands on the row drawn under it. A chosen row's tint keeps inside its own row:
        // each row insets its ground as painted.
        let [x, _, width, _] = cx.node().rect;
        let (left, right) = cx
            .node()
            .children
            .iter()
            .map(|row| {
                let start = row.rect[0];
                let end = start + row.rect[2];
                (start - x, (x + width) - end)
            })
            .reduce(|(l0, r0), (l1, r1)| (l0.min(l1), r0.min(r1)))
            .unwrap_or((0.0, 0.0));
        cx.flow(&format!(
            "display:flex;flex-direction:column;padding:0 {}px 0 {}px;",
            right.max(0.0),
            left.max(0.0)
        ));
        let chosen: Vec<vello::kurbo::Rect> = self
            .mounted
            .iter()
            .filter(|row| self.row_selection(row.index).selected)
            .map(|row| row.widget.get_rect())
            .collect();
        if !chosen.is_empty() {
            cx.choose_children(&chosen);
        }
        self.name_page_rows(&mut cx.composed.children);
        if self.tree.is_some() {
            let theme = cx.theme().clone();
            let rows = std::mem::take(&mut cx.composed.children);
            cx.composed.children = self.tree_html_rows(rows, &theme);
        }
    }

    crate::provides!(
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
        // A model that follows the app's state has its changes in the pass that changed it:
        // taken here, the rows it moved are placed in this frame, not the next.
        self.take_model_changes();
        self.take_reveal_asked();
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
        // A glide steps on the frame's instant, the one its layout and paint read the list at: on
        // the clock, a frame that came late moved the rows past where its layout had mounted them.
        let now = crate::motion::frame_now();
        let elapsed = self
            .last_tick
            .replace(now)
            .map_or(crate::time::Duration::ZERO, |last| {
                now.saturating_duration_since(last)
            });
        let mut changed = self.take_model_changes();
        changed |= self.take_reveal_asked();
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
            let mounted: Vec<ListKey> = self.mounted.iter().map(|row| row.key.clone()).collect();
            self.invalidate_rows(mounted);
        }
        self.disclosure = disclosure;
        // A tree's rows stand as an outline's ([`crate::widget::ListRow::set_outline`]).
        self.row_line = if self.tree.is_some() {
            f64::from(crate::widget::TREE_ROW_HEIGHT)
        } else {
            f64::from(theme.tokens.metrics.row_single)
        };
        self.settle_lead();
        self.pill_height =
            crate::widget::ButtonFace::height(crate::widget::ControlSize::Regular, theme);
        // A pill floats over the rows it covers, so its width is the one its words need: measured
        // here, where the face's theme is known, so a list laid out and never drawn offers the
        // words whole.
        self.pill_widths = crate::text_render::with_layout_renderer(theme, |text| {
            crate::widget::conversation::Jump::ALL.map(|jump| jump.width(text, theme))
        });
        // A way back the list no longer offers keeps no focus: what was the keyboard's is gone.
        self.jump_focused &= self.jump().is_some();
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
        // A list that fills its parent says how tall its rows come to, for a parent that has no
        // height to give and asks what it holds (`natural_style`).
        self.natural = self
            .fill_height
            .then(|| taffy.new_leaf(self.natural_style()).ok())
            .flatten();
        child_nodes.extend(self.natural);
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
                // Taking its parent's height, it can always be as short as the parent leaves it:
                // what its rows come to (`natural`) is what it says only where the parent has no
                // height to offer, never a least height it holds a pane open to.
                min_size: Size {
                    width: Dimension::auto(),
                    height: if self.fill_height {
                        Dimension::length(0.0)
                    } else {
                        Dimension::auto()
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
        let _ = taffy.set_restyled_for_width(node, !self.fit_rows);
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.structure_dirty = false;
        self.remount = false;
        self.letting_go.clear();
        self.layout_node = Some(node);
        node
    }

    /// Rows laid out for the first time are measured as the list is solved, and every row is
    /// placed where the measured rows above it put it, in the same solve. A list as tall as its
    /// rows is not: its own height follows from them, which its parent has read already.
    fn style_laid_out(
        &mut self,
        styles: &mut taffy::Restyle<'_>,
        _laid: &taffy::LayoutOutput,
    ) -> bool {
        let mut moved = false;
        for at in 0..self.mounted.len() {
            let Some(row) = self.mounted.get(at) else {
                continue;
            };
            let Some(child) = row.node else {
                continue;
            };
            if self.viewport.is_measured(&row.key) {
                continue;
            }
            let (index, key) = (row.index, row.key.clone());
            let laid = crate::widget::layout::snap_to_device_pixels(f64::from(
                styles.laid(child).size.height,
            ));
            let height = self.grouped_height(index, laid);
            let before = (
                self.viewport.item_height(index).map(f64::to_bits),
                self.viewport.content_extent().to_bits(),
            );
            let shown = self.grown_height(index, &key, height.max(1.0), true);
            self.viewport.measure(index, key, shown);
            moved |= before
                != (
                    self.viewport.item_height(index).map(f64::to_bits),
                    self.viewport.content_extent().to_bits(),
                );
        }
        if !moved {
            return false;
        }
        self.place_rows(styles);
        if let Some(natural) = self.natural {
            styles.set_style(natural, self.natural_style());
        }
        true
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
            // Rows stand at offsets from the list's top: only a scroll or a bar the height brings
            // (or rows it shows, below) lays them out again.
            let offset = self.viewport.offset();
            self.viewport.set_viewport(self.rect.height());
            self.keep_end();
            let beside_bar = self.overlay_bar.is_none() && self.scroll_metrics().overflows();
            if self.viewport.offset().to_bits() != offset.to_bits()
                || beside_bar != self.rows_beside_bar
            {
                self.visible_window_changed();
            } else {
                self.refresh_anchor();
            }
        }
        let slack = self.end_slack();
        let content = crate::widget::layout::snap_to_device_pixels(
            slack + self.layout_offset - self.viewport.offset(),
        );
        let child_origin = Point::new(self.rect.x0, self.rect.y0 + content);
        if self.fit_rows {
            self.fit_shown = crate::widget::layout_clip().map_or(FitShown::Whole, |clip| {
                FitShown::Band(clip.y0 - child_origin.y, clip.y1 - child_origin.y)
            });
        }
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
            let geometry = |viewport: &SparseVirtualViewport<ListKey>| {
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
                self.jump_hovered = self.pill_target().is_some_and(|rect| rect.contains(point));
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
                if self.pill_target().is_some_and(|rect| rect.contains(cursor)) {
                    self.jump_pressed = true;
                    *ctx.handled = true;
                    return;
                }
                // The row is the press's target even when the list is built again before the
                // release (a page beside it arriving): the list in its place completes it.
                if let Some((_, carried, rect)) = self.row_press_at_cursor() {
                    self.held_row
                        .press(crate::pointer_replay::PressAt::one(&carried, rect));
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
                        self.pill_target().is_some_and(|rect| rect.contains(point))
                    }) {
                        self.follow_pill();
                    }
                    *ctx.handled = true;
                    return;
                }
                let over = self.row_press_at_cursor();
                let carried =
                    self.held_row
                        .release(over.as_ref().map(|(_, carried, rect)| {
                            crate::pointer_replay::PressAt::one(carried, *rect)
                        }));
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
                } else if carried == crate::pointer_replay::Release::Click(())
                    && let Some((key, ..)) = over
                {
                    // A press the list built before this one took on the same row.
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

    /// A row is called by its key where the list words its keys: a row still on its way and the
    /// row that arrives in its place are one element of a page, and the rows after it keep theirs.
    fn child_path_name(&self, child_index: usize) -> Option<String> {
        let encode = self.semantic_key.as_ref()?;
        let row = self.mounted.get(child_index)?;
        Some(format!("row[{}]", path_safe(&encode(&row.key))))
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

    fn paint_bounds(&self) -> Rect {
        self.drawn()
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
            regions.push(self.drawn());
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
                    regions.push(self.drawn());
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

    fn has_pending_visual_work(&self) -> bool {
        self.arrivals.revealing(crate::time::now())
            || super::defaults::has_pending_visual_work(&|visit| {
                crate::widget::Widget::for_each_child(self, visit);
            })
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
            heads_rows: groups::group_of(&*self.model, 0, self.tail_title.as_deref()).is_some(),
            lead: crate::num::f32_of_f64(self.viewport.edges().0),
            pill: self.pill_layout(),
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

/// `key` as one part of a widget path: the characters that separate a path's parts (`/`, `#`) and
/// the one that escapes them stand as escapes.
fn path_safe(key: &str) -> String {
    key.replace('%', "%25")
        .replace('/', "%2F")
        .replace('#', "%23")
}
