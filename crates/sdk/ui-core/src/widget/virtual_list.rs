//! Sparse, variable-height list with retained row delegates.
//!
//! The model exposes stable keys and random access without requiring all rows
//! to be resident. Only the viewport window owns widget instances; those
//! instances survive reorder while their key remains visible.

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
mod lift;
#[path = "virtual_list_press.rs"]
mod press;
#[path = "virtual_list_row_group.rs"]
mod row_group;
pub use row_group::RowGroup;

const DEFAULT_VIEWPORT_HEIGHT: f64 = 320.0;
/// How near its end a list that follows its end still counts as at it, in logical pixels.
const END_SLACK: f64 = 0.5;

fn px(value: f64) -> f32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "screen-space logical pixels are bounded far below f32::MAX"
    )]
    let value = value as f32;
    value
}

/// One already-applied change to a [`ListModel`]. Structural variants retain
/// mounted delegates by stable key; [`Self::RowsChanged`] rebuilds only the
/// named rows.
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

/// Random-access collection contract for [`VirtualList`]. Keys must be
/// unique and stable while an item represents the same logical entity.
pub trait ListModel {
    type Key: Clone + Eq + Hash + 'static;

    fn len(&self) -> usize;
    fn key_at(&self, index: usize) -> Option<Self::Key>;
    fn index_of(&self, key: &Self::Key) -> Option<usize>;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Where a logical row's data stands: [`Arrival::Pending`] (saying where
    /// it comes from), ready, or failed with the reason. A paged model keeps
    /// stable keys and geometry while its rows arrive; the list reserves a
    /// pending row's slot and paints its placeholder there
    /// ([`crate::widget::ArrivingRows`]), and builds the row's delegate only
    /// once it is ready.
    fn row_state(&self, _index: usize) -> Arrival<(), &str> {
        Arrival::Ready(())
    }

    /// Whether row `index`, failed, can be asked for again: its failure
    /// then offers a retry, which calls [`Self::retry`].
    fn can_retry(&self, _index: usize) -> bool {
        false
    }

    /// Ask again for row `index` after it failed.
    fn retry(&mut self, _index: usize) {}

    /// Opportunity to enqueue work for the visible window plus overscan.
    fn request_range(&mut self, _range: Range<usize>) {}

    /// Drain a model-owned invalidation wakeup. Return true after logical
    /// order, count, key, or row-state changes. Use
    /// [`VirtualList::invalidate_rows`] as well when ready delegate content
    /// changed without changing state.
    fn poll_changed(&mut self) -> bool {
        false
    }

    /// Drain specific model changes. Prefer this to [`Self::poll_changed`]
    /// when an asynchronous source can identify its batch: stable delegates
    /// then survive structural changes and only named content rows rebuild.
    fn poll_changes(&mut self) -> Vec<ListModelChange<Self::Key>> {
        Vec::new()
    }

    /// Keep scheduling frames while [`Self::poll_changed`] or
    /// [`Self::poll_changes`] may observe an in-flight asynchronous source.
    fn needs_poll(&self) -> bool {
        false
    }

    /// The name a person reads row `index` by, which typing its first
    /// letters in the focused list moves to ([`crate::type_ahead`]). `None`
    /// for a row without one, or a model that names no rows.
    fn label(&self, _index: usize) -> Option<String> {
        None
    }

    /// The key of the row showing the resource `id`, when the model can say: a host that
    /// renders rows as they show offers this ([`crate::component::RowSource`]).
    fn key_presenting(&self, _id: &str) -> Option<Self::Key> {
        None
    }

    /// The item the row `key` shows, as the app names a row's item: what a row keeps when the
    /// app names the list's rows again under keys of their own. `None` when the model cannot
    /// say, or the row shows no item.
    fn item_presented(&self, _key: &Self::Key) -> Option<String> {
        None
    }

    /// What the row `index` is grouped by, when the model groups its rows: the list draws the
    /// heading over the first row of each group ([`RowGroup`]), a disclosure the keyboard
    /// reaches like a row and assistive technology reads as one. `None` for a row in no group,
    /// or a model that groups nothing.
    fn group_at(&self, _index: usize) -> Option<RowGroup> {
        None
    }

    /// What the row stands as in a tree ([`VirtualList::with_tree`]): the row it is under, and
    /// what the list shows of its subtasks. `None` for a row outside a tree, or a model that
    /// holds none.
    fn tree_at(&self, _index: usize) -> Option<TreeRow<Self::Key>> {
        None
    }

    /// Whether the row `index` takes other rows as its subtasks: a tree's drag offers a drop
    /// into it ([`DropPlacement::Inside`]), and the keyboard indents a row into it. `false`
    /// for a leaf, or a model that holds no tree.
    fn accepts_children(&self, _index: usize) -> bool {
        false
    }

    /// Whether the row `index` can be renamed in place ([`VirtualList::on_rename`]): `F2`, a
    /// double click and [`VirtualList::begin_rename`] open a field only over a row that does.
    fn renames(&self, _index: usize) -> bool {
        true
    }

    /// Whether the row `index` has subtasks to disclose, whether or not any of them stands in
    /// the list: a folder read when it opens shows the disclosure that reads it. `false` for
    /// a leaf, or a model that holds no tree.
    fn has_subtasks(&self, _index: usize) -> bool {
        false
    }

    /// Whether the list heads or nests its rows, keeping a disclosure gutter left of its column.
    fn discloses(&self) -> bool {
        false
    }

    /// Whether the row `index` opens into its item where it stands: the row carries the
    /// item's document under it, and choosing the row opens it there rather than anywhere
    /// else. `false` for a row that opens elsewhere, or a model whose rows open nothing.
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
/// Where a row stands for the people in it, or reads as standing: the location a
/// collaborator's presence is painted at.
type RowPresence<M> =
    Box<dyn Fn(&M, usize, &<M as ListModel>::Key) -> Option<crate::presence::PresenceKey>>;
type SelectCallback<K> = Box<dyn FnMut(K, bool)>;
/// Told the rows selected, in the order they stand: every row of a multi-selection, after
/// anything changed it.
type SelectedCallback<K> = Box<dyn FnMut(&[K])>;

/// Whether what the list holds now is a choice the person made by hand, or only the row the
/// focus moved to: a list whose selection follows the focus moves it on every arrow, and a
/// row opened onto moves it too, and those are not choices a window keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Choosing {
    /// A click, a toggle, a range, a select-all, or Escape: the person built the choice.
    ByHand,
    /// The choice followed the focus or the row that opened.
    Following,
}
type ReorderValidator<M> =
    Box<dyn FnMut(&M, &<M as ListModel>::Key, &DropTarget<<M as ListModel>::Key>) -> bool>;
type ReorderCommit<M> = Box<dyn FnMut(&mut M, ReorderDrop<<M as ListModel>::Key>) -> bool>;
use super::row_selection::selection_sink;
pub use super::row_selection::{
    PaintedRowLifts, RowBuild, RowContentBox, RowLift, RowSelection, RowSelectionSink,
    row_content_box,
};

struct MountedRow<K> {
    key: K,
    index: usize,
    /// Which build of the row's widget this is: a row whose widget was built
    /// anew (its content or its selection changed) carries a new stamp, and the
    /// accessibility walk builds its node again.
    stamp: u64,
    state: MountedRowState,
    selection: RowSelection,
    widget: Element,
    node: Option<NodeId>,
    /// The row this one was built to replace, in the layout the host
    /// retains: it stays alive, and stands in the layout walk where this one
    /// will, until the remount lets go of its node
    /// ([`VirtualList::remount_rows`]) or a build drops it.
    replacing: Option<LaidOutRow>,
}

/// A row's widget as the retained layout holds it, and its node there.
struct LaidOutRow {
    widget: Element,
    node: NodeId,
}

impl<K> MountedRow<K> {
    /// The widget the retained layout holds for this row: its own when it
    /// was laid out, else the one it replaces until the next remount.
    fn laid_out(self) -> Option<LaidOutRow> {
        match self.node {
            Some(node) => Some(LaidOutRow {
                widget: self.widget,
                node,
            }),
            None => self.replacing,
        }
    }

    /// The node the retained layout holds for this row, if any.
    fn laid_out_node(&self) -> Option<NodeId> {
        self.node
            .or_else(|| self.replacing.as_ref().map(|row| row.node))
    }

    /// The widget the retained layout walk meets for this row.
    fn laid_out_widget(&self) -> &dyn Widget {
        match (&self.node, &self.replacing) {
            (None, Some(row)) => row.widget.as_ref(),
            _ => self.widget.as_ref(),
        }
    }
}

/// What a mounted row's widget stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum MountedRowState {
    /// The delegate's row.
    Ready,
    /// The reserved slot of a row on its way.
    Pending,
    /// Why the row could not be read, and whether it offers a retry.
    Failed(String, bool),
}

/// The space a list leaves above its first row and below its last
/// ([`VirtualList::with_edges`]), part of what it scrolls: a page's top and
/// bottom spacing around rows that are a page's content, one by one.
pub trait ListEdges {
    /// Read what the edges depend on from the theme of a layout.
    fn theme(&mut self, theme: &Theme);

    /// The space above the first row and below the last, for rows `width`
    /// wide.
    fn edges(&mut self, width: f64) -> (f64, f64);
}

/// A row widget says where its own hover actions stand, so the list reports the place to press
/// one without knowing what the host built its row of ([`VirtualList::row_action_rect`]). A row
/// without actions provides none; [`ListRow`](crate::widget::ListRow) provides its own.
pub trait RowActionRects {
    /// Where the row's hover actions stand on screen, in the order it shows them: what a host
    /// presses to reach one from a script or a test.
    fn row_action_rects(&self) -> Vec<Rect>;
}

/// Where a list's rows stand between its edges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowStance {
    /// One lift's bleed in, so their lift fills the list.
    Bleed,
    /// Edge to edge: rows that bring their own insets ([`VirtualList::flush`]).
    Flush,
    /// On the column's content edge ([`VirtualList::in_column`]).
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
    /// The delegate reads [`RowSelection`]: a mounted row is rebuilt when
    /// its own or a neighbour's selection changes.
    selection_aware: bool,
    /// Where each row stands for the people in it: its collaborators' presence is painted
    /// there, so a list shows where everyone is.
    row_presence: Option<RowPresence<M>>,
    /// The rows keep their tooltips away: something the host paints covers the list.
    tooltips_suppressed: bool,
    /// The delegate paints the selection; the list paints only focus.
    delegate_selection: bool,
    viewport: SparseVirtualViewport<M::Key>,
    mounted: Vec<MountedRow<M::Key>>,
    /// Rows the list let go of in a reconcile whose nodes the retained
    /// layout still holds, at the place they stood: they keep the layout
    /// walk's place for the rows that replace them, and their widgets
    /// alive, until a remount unmounts their nodes.
    letting_go: Vec<(usize, LaidOutRow)>,
    selection: KeySelection<M::Key>,
    multi_select: bool,
    /// The rows are read, not chosen ([`Self::reading`]): no gesture makes one the selection.
    reading: bool,
    /// Moving the focused row by keyboard (or assistive technology) also
    /// selects it.
    follow_focus: bool,
    focused_key: Option<M::Key>,
    /// The group's heading holding the keyboard (the second is the index of the first row
    /// under it): a heading is a stop of the list's own, opened from the keyboard.
    focused_group: Option<(String, usize)>,
    /// What has been typed into the focused list ([`ListModel::label`]).
    typing: crate::type_ahead::TypeAhead,
    on_select: Option<SelectCallback<M::Key>>,
    /// Told the whole selection after it changed, when it can hold more than one row.
    on_selected: Option<SelectedCallback<M::Key>>,
    /// What the last report to [`Self::on_selected`] held more than one row: the way back from
    /// a choice of many to one or none is kept even when it is one row.
    reported_many: bool,
    /// Choosing a row, published as the list's view action
    /// ([`VirtualList::on_view`]).
    view: Option<(crate::widget::view_choice::ViewChoice, KeyCallback<M::Key>)>,
    /// Opening a row, published as the list's view action
    /// ([`VirtualList::on_activate_view`]).
    opens: Option<(crate::widget::view_choice::ViewChoice, KeyCallback<M::Key>)>,
    on_activate: Option<KeyCallback<M::Key>>,
    /// A press on a row opens it, without waiting for a second one
    /// ([`VirtualList::open_on_press`]).
    open_on_press: bool,
    /// A row's subtasks folded or opened ([`VirtualList::on_fold`]).
    on_fold: Option<FoldCallback<M::Key>>,
    /// Told the item the person opened where it stands ([`VirtualList::on_opened`]), and told
    /// that it closed ([`VirtualList::on_closed`]).
    on_opened: Option<KeyCallback<M::Key>>,
    on_closed: Option<Box<dyn FnMut()>>,
    /// The row standing open into its item where it stands: a row that
    /// [`opens in place`](ListModel::opens_in_place) opens here when it is chosen, and an item
    /// that leaves the list closes with it ([`Self::set_open`]). One row stands open at a
    /// time, and the row builder is told which row it is drawing.
    open: Option<M::Key>,
    /// The row that stood open when the app named the list's rows again
    /// ([`ListModelChange::Reset`]): its keys are read anew, and until they arrive the row is
    /// not one that left ([`Self::close_open_leaving`]).
    rereading_open: Option<M::Key>,
    /// Rows whose measured height is on its way to another, at the height they show now: a row
    /// that grew into its item, or whose content changed, moves the rows after it as it
    /// arrives, and the list clips it to the height it shows.
    growth: Vec<(M::Key, crate::motion::MotionValue<f64>)>,
    /// The rows whose height the list is moving on purpose, because it opened or closed them
    /// into their item: a row that changed for any other reason stands at its height at once.
    growing: HashSet<M::Key>,
    /// The menu the rows and the list's own space open, or the host's report of a row's menu
    /// ([`VirtualList::with_context_menu`], [`VirtualList::with_row_menu_reporter`]).
    menus: RowMenus<M>,
    /// The last row a click landed on and when, for a double click.
    clicks: super::rows::Clicks<M::Key>,
    semantic_key: Option<SemanticKey<M::Key>>,
    accessible_name: Option<String>,
    rect: Rect,
    last_cursor: Option<Point>,
    is_focused: bool,
    drag_scroll: DragScroller,
    /// The row a left press landed on. A row takes the press for a gesture of its own (a
    /// swipe) and the release is still a click on that row when it was a tap, so the press
    /// is kept until the release, whoever the press went to.
    pressed_row: Option<(usize, M::Key)>,
    reorder: ReorderController<M::Key>,
    reorder_validator: Option<ReorderValidator<M>>,
    reorder_commit: Option<ReorderCommit<M>>,
    scroll: super::rows::RowScroll,
    /// Whether a hairline runs up each depth's gutter through nested rows
    /// ([`VirtualList::with_indent_guides`]).
    indent_guides: bool,
    /// The motion the theme allows, sampled by the latest draw.
    motion: crate::tokens::EffectiveMotion,
    /// The pace the list's own motion plays at, when it says something other than the theme's.
    feel: crate::motion::Feel,
    reduce_motion: bool,
    vbar: Scrollbar,
    /// The bar laid over the rows' trailing edge and faded out at rest, in place of the bar
    /// standing in a gutter beside them ([`Self::overlay_scrollbar`]).
    overlay_bar: Option<crate::widget::OverlayScrollbar>,
    /// `metrics.scrollbar_gutter` and the row lift's bleed, from the last
    /// layout's theme.
    gutter: f64,
    /// The rounded corners of the window this sits in, learned where it
    /// last drew: its bars keep out of them.
    corners: crate::widget::scrollbar::SurfaceCorners,
    bleed: f64,
    /// Where the rows' content starts in from the list's edge: its bleed, or a disclosure gutter.
    lead: f64,
    /// The gutter a disclosure chevron hangs in, one step of nesting.
    disclosure: f64,
    /// How far a row's lift bleeds past its text ([`crate::chrome::row_bleed`]).
    lift: f64,
    /// The selection lifts the last painted frame drew for its rows, and what those rows show
    /// ([`crate::widget::RowLift`]): an audit holds a lift to the row it selects.
    painted_lifts: Vec<crate::widget::RowLift>,
    /// Whether a row with an item opens into it where it stands, or the item opens in front of
    /// the list ([`Self::in_place`]).
    in_place: bool,
    /// The height of a row's own line, from the last layout's theme: what a row that grew into
    /// its item still shows where it starts.
    row_line: f64,
    /// Where the rows stand between the list's edges ([`Self::flush`], [`Self::in_column`]).
    stance: RowStance,
    /// The space above the first row and below the last ([`Self::with_edges`]).
    edges: Option<Box<dyn ListEdges>>,
    /// Whether the rows were last laid out beside a scroll bar.
    rows_beside_bar: bool,
    /// The list takes its parent's height, and its viewport follows it.
    fill_height: bool,
    /// The list is as tall as its rows ([`Self::fitting_rows`]).
    fit_rows: bool,
    /// The list follows its end, as a transcript does ([`Self::following_end`]).
    follow_end: bool,
    /// Whether a list that follows its end shows it: the last scroll left it there.
    at_end: bool,
    last_tick: Option<Instant>,
    last_anchor: Option<SparseAnchor<M::Key>>,
    requested_range: Option<Range<usize>>,
    request_dirty: bool,
    structure_dirty: bool,
    /// The list's own box changed height, as a list that fits its rows measures taller
    /// ones: the page around it lays out again.
    box_dirty: bool,
    layout_dirty: bool,
    /// The view moved since the rows were last placed: a layout pass places
    /// them again, measuring nothing (as `Scrollable::scroll_dirty`).
    scroll_dirty: bool,
    interaction: crate::interaction::InteractionEnvironment,
    /// The scroll offset the rows' layout was built at: a scroll that brings
    /// no row near moves them in `apply_layout`, without a rebuild.
    layout_offset: f64,
    /// The rows mounted at the last reconcile ([`Self::mount_window`]),
    /// and whether a scroll since brought others near: a rebuild that only
    /// mounts and lets go of rows is one the frame's damage describes
    /// ([`crate::widget::StructureChange::Described`]).
    mounted_range: Range<usize>,
    remount: bool,
    /// Mounted rows whose content changed ([`Self::invalidate_rows`]): the
    /// next reconcile builds them anew, where they stand.
    invalidated: HashSet<M::Key>,
    /// Counts the row widgets built: a row's stamp, so the accessibility walk
    /// tells a row built anew from one nothing changed about.
    row_builds: u64,
    /// The row nodes the last accessibility walk kept, handed over again while
    /// nothing about the row changed ([`super::virtual_list_a11y::KeptAccessRows`]).
    access_rows: std::cell::RefCell<a11y::KeptAccessRows<M::Key>>,
    /// The list's node in the layout it last built, where its host lays out
    /// the rows a scroll brings near in place ([`Widget::remount_children`]).
    layout_node: Option<NodeId>,
    /// What the frame being drawn shows, and what the last frame the host
    /// presented showed: a scroll between the two that changed nothing else
    /// is offered to the host as a copy ([`crate::scroll_copy::offer`]).
    shown: crate::scroll_copy::ShownFrames<ListShown>,
    /// Rows moving or fading after a change the person made, and a
    /// structural change waiting for the next frame to start their motion.
    row_motion: crate::collection_transition::RowMotions<M::Key>,
    /// The placeholders of rows on their way.
    arrivals: ArrivingRows<M::Key>,
    /// The failed row whose retry was pressed, for the next update.
    retry_asked: Rc<Cell<Option<usize>>>,
    /// The mounted rows the last placement placed, when it placed only
    /// those near the box ([`crate::widget::placing_again`]); the others
    /// keep earlier boxes until they come near. `None`: every row, and the
    /// placement that found no row near the box, which placed none to
    /// narrow to.
    placed: Option<Range<usize>>,
    /// The rows near the box at the last placement, placed as they leave.
    near_placed: Range<usize>,
    /// The item of a transcript the reader has not seen
    /// ([`crate::widget::conversation::Jump`]): a "New messages" divider stands over its row.
    /// Kept by the item's id, not its row's key: a host renders rows as they show, so the key
    /// is worked out when it is needed ([`Self::unread_from`]).
    unread: Option<String>,
    /// Where the pill over a transcript was pressed, and whether the pointer is on it.
    jump_hovered: bool,
    jump_pressed: bool,
    /// The pill's height and widest width, from the last layout: a press and the pointer test
    /// its rectangle without a theme.
    pill_height: f64,
    pill_width: f64,
    /// How an item becomes the key of the row showing it, for a model that names none.
    names_item: std::cell::RefCell<Option<Box<dyn Fn(&str) -> Option<M::Key>>>>,
    /// The groups of the rows the model names: which are open, and the bands the last layout
    /// placed for their headings.
    groups: Groups,
    /// The title over the list's tail group ([`RowGroup::Tail`]).
    tail_title: Option<String>,
    /// What each row is grouped by, by key: the model is asked once for a row while its props
    /// stand as they were.
    row_groups: HashMap<M::Key, Option<groups::Group>>,
    /// The rows are a tree ([`Self::with_tree`]): each under the row of the item it names as
    /// its parent, its subtasks folded away where the person folded them.
    tree: Option<Rc<TreeMarks<M::Key>>>,
    /// The tree's rows as the model gives them, read once each while they stand.
    tree_rows: HashMap<M::Key, TreeRow<M::Key>>,
    /// The rows standing under a folded row: they take no room and are never built.
    tree_hidden: HashSet<M::Key>,
    /// A row renamed in place: the field over its label, its title reported as it commits
    /// ([`Self::on_rename`]).
    rename: Option<Box<RenameField<M::Key>>>,
    on_rename: Option<RenameCallback<M::Key>>,
    /// A double click on a row renames it rather than opening it, as an outliner does.
    rename_on_double_click: bool,
    /// `Tab` and `Shift+Tab` indent and outdent the focused row, as an outliner does.
    tab_indents: bool,
    /// A row the person asked to delete (`Delete` on the focused row).
    on_delete: Option<KeyCallback<M::Key>>,
}

impl<M: ListModel> VirtualList<M> {
    /// A list of `model`'s rows, each built by `row_builder` once its data
    /// is there. A row still on its way ([`ListModel::row_state`]) keeps a
    /// reserved slot of its last or estimated height, with its placeholder
    /// painted by the list; a failed row shows why, with a retry when the
    /// model offers one. The builder never sees either.
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

    /// Build a list whose delegate paints its own selection from the
    /// [`RowSelection`] it receives (a run of selected rows as one lift, a
    /// separator hidden beside a selected row). The list rebuilds a mounted
    /// row when its own or a neighbour's selection changes, and paints only
    /// the keyboard focus. A [`ListRow`](crate::widget::ListRow) delegate
    /// takes the selection without being rebuilt.
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
            disclosure: f64::from(
                crate::tokens::Metrics::BASE.icon_sm + crate::tokens::Spacing::BASE.rhythm.related,
            ),
            lift: f64::from(crate::tokens::Spacing::BASE.s2),
            painted_lifts: Vec::new(),
            in_place: true,
            row_line: f64::from(crate::tokens::Metrics::BASE.row_single),
            stance: RowStance::Bleed,
            edges: None,
            rows_beside_bar: false,
            fill_height: false,
            fit_rows: false,
            follow_end: false,
            at_end: false,
            last_tick: None,
            last_anchor: None,
            requested_range: None,
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
            row_groups: HashMap::new(),
            tree: None,
            tree_rows: HashMap::new(),
            tree_hidden: HashSet::new(),
            rename: None,
            on_rename: None,
            rename_on_double_click: false,
            tab_indents: false,
            on_delete: None,
        };
        list.refresh_anchor();
        list
    }

    /// Scroll the row of `key` into view and make it the focused row. A
    /// reveal the person's hand or key just asked for (a search hit opened,
    /// a row picked elsewhere) moves there ([`crate::motion::ScrollTo`]):
    /// a short jump glides, a long one fades in at the row; one nobody
    /// asked for lands at once.
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

    /// Follow a reveal in progress to where it is drawn at `now`.
    fn follow_scroll_to(&mut self, now: Instant) {
        if let Some(at) = self.scroll.follow(now) {
            self.set_scroll_offset(at);
        }
    }

    /// A reveal still moves, or fades the rows in after a long jump.
    /// The frame after the fade arrives draws the rows whole again.
    fn reveal_in_progress(&self, now: Instant) -> bool {
        self.scroll.to.target().is_some()
            || self.scroll.to.is_moving(now)
            || self.shown.drawing().is_some_and(|shown| shown.in_motion)
    }

    /// Give this list a stable app-test/inspection id.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    /// Play the list's own motion — rows arriving, leaving and changing places, the reveal that
    /// takes the person to one — at `feel` rather than the theme's pace.
    #[must_use]
    pub fn with_feel(mut self, feel: crate::motion::Feel) -> Self {
        self.feel = feel;
        self
    }

    /// [`Self::with_feel`] on a list already built.
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

    /// Rows span the list, edge to edge: for rows that bring their own insets
    /// and lift no selection, as a page's rows do, so their content shares
    /// the edge of the page around the list. Rows of a list stand one lift's
    /// bleed in from its edges otherwise.
    /// The scroll bar lies over the rows' trailing edge, shown while they scroll or the
    /// pointer is at it and faded out at rest, and the rows keep their whole width: for a
    /// sidebar, whose rows read as one column with the pane beside it.
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

    /// The list is a column's whole body, edge to edge in it: its rows' text starts on the
    /// column's content edge ([`crate::chrome::column_gutter`]), where the column's header
    /// starts its title, and their lift bleeds [`crate::chrome::row_bleed`] past it, as HIG's
    /// one content edge has every list in a column do. The scroll bar stays on the column's
    /// edge. Rows lift themselves, as a [`ListRow`](crate::widget::ListRow) does.
    #[must_use]
    pub const fn in_column(mut self) -> Self {
        self.stance = RowStance::Column;
        self
    }

    /// Leave the space `edges` asks for above the first row and below the
    /// last. It scrolls with the rows, as a page's spacing does.
    #[must_use]
    pub fn with_edges(mut self, edges: impl ListEdges + 'static) -> Self {
        self.edges = Some(Box::new(edges));
        self
    }

    /// Bring the space above the first row and below the last in line with
    /// the rows' width. Whether it changed.
    fn update_edges(&mut self) -> bool {
        let width = (self.rect.width() - self.lead - self.trailing_inset()).max(0.0);
        let Some(edges) = self.edges.as_mut() else {
            return false;
        };
        let (lead, trail) = edges.edges(width);
        self.viewport.set_edges(lead, trail)
    }

    /// How far the rows stand in from the list's trailing edge: as far as from its leading
    /// edge, and clear of the bar's gutter beside it. A column's rows already leave the bar
    /// room in the column's gutter when it is wide enough for it and their lift.
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

    /// Take the parent's full height (a pane's one list) instead of a fixed
    /// viewport; the viewport follows the laid-out height.
    #[must_use]
    pub fn filling(mut self) -> Self {
        self.fill_height = true;
        self
    }

    /// Stand as tall as its rows, among the other content of a page that scrolls them all (a
    /// task's steps above its notes), rather than taking its parent's height and scrolling them
    /// itself: every row shows, and what follows the list stands after its last row.
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

    /// Select more than one row, or one again.
    pub fn set_multi_select(&mut self, enabled: bool) {
        self.multi_select = enabled;
    }

    /// Told the rows selected, in the order they stand, after anything changed the selection:
    /// a click with its modifiers, the keyboard, `Escape` clearing it, or a row opened onto.
    #[must_use]
    pub fn on_selected(mut self, callback: impl FnMut(&[M::Key]) + 'static) -> Self {
        self.set_on_selected(callback);
        self
    }

    /// [`Self::on_selected`] on a list already built: what the list reports is the callback
    /// standing now, so a list whose items are given again tells its new host.
    pub fn set_on_selected(&mut self, callback: impl FnMut(&[M::Key]) + 'static) {
        self.on_selected = Some(Box::new(callback));
    }

    /// Select the row the keyboard moves to, as a single-selection list
    /// whose selection is what it shows elsewhere (the open item of a
    /// master-detail window) does. A modifier-extended move still extends.
    #[must_use]
    pub fn selection_follows_focus(mut self) -> Self {
        self.follow_focus = true;
        self
    }

    /// The rows are read, not chosen: a press or a tap on one gives it the keyboard and
    /// chooses nothing, so no row is left tinted behind the finger, and the keyboard's way
    /// through the rows rings the row it is on without choosing that one either. A list that
    /// shows what people said — a conversation's messages, a document's replies, the rows of a
    /// page column — is read this way; a list that stands for the things a window opens is
    /// chosen.
    #[must_use]
    pub const fn reading(mut self) -> Self {
        self.reading = true;
        self
    }

    /// Restore stable-key selection and discard keys absent from this model.
    #[must_use]
    pub fn with_selection(mut self, selection: KeySelection<M::Key>) -> Self {
        self.set_selection(selection);
        self
    }

    /// [`Self::with_selection`] on a list already built: the rows chosen now, keys absent from
    /// the model dropped, and the rows on screen show it where they stand.
    pub fn set_selection(&mut self, mut selection: KeySelection<M::Key>) {
        selection.reconcile(|key| self.model.index_of(key).is_some());
        self.reported_many = selection.is_select_all() || selection.stored_keys().count() > 1;
        self.selection = selection;
    }

    /// How many of the rows the model holds the selection names.
    pub fn selection_len(&self) -> usize {
        self.selection.len(self.model.len())
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    /// Where each row stands for the people in it: the list paints the collaborators
    /// present at `presence`'s location over the row, in their colours, so a list shows
    /// where everyone is (`crate::presence`).
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

    /// [`Self::on_select`] on a list already built.
    pub fn set_on_select(&mut self, callback: impl FnMut(M::Key, bool) + 'static) {
        self.on_select = Some(Box::new(callback));
    }

    #[must_use]
    pub fn on_activate(mut self, callback: impl FnMut(M::Key) + 'static) -> Self {
        self.set_on_activate(callback);
        self
    }

    /// [`Self::on_activate`] on a list already built.
    pub fn set_on_activate(&mut self, callback: impl FnMut(M::Key) + 'static) {
        self.on_activate = Some(Box::new(callback));
    }

    /// Told the item the person opened where it stands. A host keeping which item is open puts
    /// it back as the list's `open` prop, so a list built again shows it.
    #[must_use]
    pub fn on_opened(mut self, callback: impl FnMut(M::Key) + 'static) -> Self {
        self.set_on_opened(callback);
        self
    }

    /// [`Self::on_opened`] on a list already built.
    pub fn set_on_opened(&mut self, callback: impl FnMut(M::Key) + 'static) {
        self.on_opened = Some(Box::new(callback));
    }

    /// Told the row standing open closed: the person pressed it again, pressed Escape, pressed
    /// beside it, or its item left the list.
    #[must_use]
    pub fn on_closed(mut self, callback: impl FnMut() + 'static) -> Self {
        self.set_on_closed(callback);
        self
    }

    /// [`Self::on_closed`] on a list already built.
    pub fn set_on_closed(&mut self, callback: impl FnMut() + 'static) {
        self.on_closed = Some(Box::new(callback));
    }

    /// Told when a row's subtasks fold away or open, which way it went, however they were
    /// folded: its disclosure, the keyboard, or assistive technology. A host keeping the
    /// person's folding (a session's open folders) hears it here.
    #[must_use]
    pub fn on_fold(mut self, callback: impl FnMut(&M::Key, bool) + 'static) -> Self {
        self.on_fold = Some(Box::new(callback));
        self
    }

    /// Encode a model key as a stable, unique string for accessibility and
    /// headless row actions. The string is opaque outside this list; it must
    /// remain unchanged while the key represents the same logical item.
    #[must_use]
    pub fn with_semantic_key(mut self, encode: impl Fn(&M::Key) -> String + 'static) -> Self {
        self.semantic_key = Some(Box::new(encode));
        self
    }

    /// The row the name `encoded` gives: the key it encodes, which is the name the accessibility
    /// tree keys the row by and an agent chooses a row of a list by ([`Self::with_semantic_key`]).
    fn key_for_name(&self, encoded: &str) -> Option<M::Key> {
        let encode = self.semantic_key.as_ref()?;
        (0..self.model.len())
            .filter_map(|index| self.model.key_at(index))
            .find(|key| encode(key) == encoded)
    }

    /// Enable row reordering. `validate` controls which stable-key targets
    /// receive a preview; `commit` mutates the model after a valid drop and
    /// returns whether the collection changed. A drag moves a row, and so
    /// does Ctrl+Up or Ctrl+Down (Alt+Up or Alt+Down, as a tree's rows
    /// move) on the focused row, through the same `validate` and `commit`.
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

    /// Hide the mounted rows' tooltips while something the host paints covers the list, as the
    /// tree does; the rows the list builds after this follow it.
    pub fn suppress_tooltips(&mut self, suppressed: bool) {
        if self.tooltips_suppressed == suppressed {
            return;
        }
        self.tooltips_suppressed = suppressed;
        self.apply_row_tooltips();
    }

    /// Hand every mounted row the tooltip state the list keeps.
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

    /// The row `key` takes the keyboard: a heading that had it lets go.
    /// Give the row of `key` the keyboard, as a click or a reveal does; the selection
    /// stays where it was unless the list follows the focus.
    pub fn focus_row(&mut self, key: M::Key) {
        self.focused_group = None;
        self.focused_key = Some(key);
    }

    /// The layout node of the mounted row wearing the keyboard, when that row takes focus.
    fn focused_row_node(&self) -> Option<NodeId> {
        self.mounted
            .iter()
            .find(|row| Some(&row.key) == self.focused_key.as_ref() && row.widget.is_focusable())
            .and_then(|row| row.node)
    }

    /// After a scroll: whether it left a list that follows its end at the end.
    fn note_scrolled(&mut self) {
        if self.follow_end {
            self.at_end = self.viewport.max_offset() - self.viewport.offset() <= END_SLACK;
        }
    }

    /// Bring a list that follows its end back to it, after its rows or its height changed.
    /// Whether that scrolled.
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

    /// Scroll row `index` into view, as the keyboard focus or assistive technology asks.
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

    /// The model indexes of the rows the list builds now, in order. What the
    /// list exports as its children is what it binds controls for: both read
    /// this, so a row reaches a screen reader and a control search alike.
    pub fn mounted_rows(&self) -> impl Iterator<Item = usize> + '_ {
        self.mounted.iter().map(|row| row.index)
    }

    #[must_use]
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        self.viewport.visible_range()
    }

    /// The rows the list holds mounted: what is on screen, up to the rows the model
    /// holds. Rows still arriving add theirs when they land
    /// ([`ListModel::needs_poll`]); until then there is nothing to mount past them,
    /// so the list asks for no remount it could never carry out.
    fn rows_to_hold(&self) -> std::ops::Range<usize> {
        let near = self.viewport.visible_range();
        let count = self.model.len();
        near.start.min(count)..near.end.min(count)
    }

    pub fn set_scroll_offset(&mut self, offset: f64) {
        self.viewport.set_offset(offset);
        // The rows move in `apply_layout`, measured as they were; rows the
        // scroll brought near mount in the pass after (`remount_rows`).
        self.note_scrolled();
        self.scroll_dirty = true;
        self.refresh_anchor();
    }

    /// What of the list's own paint the rows do not carry: the selection
    /// it paints for them, its focus ring, a reorder in flight.
    fn chrome_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.is_focused.hash(&mut hasher);
        self.focused_key.hash(&mut hasher);
        self.delegate_selection.hash(&mut hasher);
        self.reorder.is_active().hash(&mut hasher);
        self.selection.is_select_all().hash(&mut hasher);
        // The selection whatever its order: each key's own hash, combined.
        let selected = self.selection.stored_keys().fold(0_u64, |combined, key| {
            let mut one = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut one);
            combined ^ one.finish()
        });
        selected.hash(&mut hasher);
        hasher.finish()
    }

    /// A stable hash of `key`, for telling a row again in a later frame.
    fn key_hash(key: &M::Key) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    /// The first mounted row on screen, as [`ListShown::first_shown`] holds
    /// it.
    fn first_shown_row(&self) -> Option<(usize, u64, f64)> {
        let give = self.give();
        self.mounted.iter().find_map(|row| {
            let top = self.viewport.item_view_top(row.index)? - give;
            let height = self.viewport.item_height(row.index)?;
            (top + height > 0.0).then(|| (row.index, Self::key_hash(&row.key), top))
        })
    }

    /// How far the first row the last presented frame showed has moved on
    /// screen since, while it is still that row.
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

    /// How far past an end fingers pull the rows, as drawn: negative before
    /// the start ([`crate::motion::RubberBand`]).
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

    /// The key of the row at `point` (window coordinates), when one is drawn there.
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
        // A row that stands in a tree takes a drop into it, and the seam below an open branch
        // at the row it holds ([`Self::tree_candidate`]).
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

    /// The selection after the keyboard moved to `key`: extended to it, or
    /// moved onto it when the selection follows focus.
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
        // The arrow keys walk the headings and the rows alike ([`Stop`]).
        if let Some(moved) = self.move_stop(key, extend) {
            return moved;
        }
        if let Some(RowStep::Page(forward)) = super::rows::row_step(key, current, count) {
            return self.page_focus(forward, extend);
        }
        // Left and right fold a tree row as its disclosure does under the pointer: right opens
        // the focused branch or steps into what is under it, left folds it away or steps out to
        // the row it stands under.
        if !toggle && !extend {
            match key {
                Key::Named(NamedKey::ArrowRight) => return self.fold_step(true),
                Key::Named(NamedKey::ArrowLeft) => return self.fold_step(false),
                _ => {}
            }
        }
        match key {
            // A heading holding the keyboard folds and unfolds its group, as its band does
            // under the pointer.
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
            // A row renamed in place, as an outliner does.
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
            // Escape closes the row standing open into its item before it puts a selection
            // away: the document closes; the row it stood on stays the row the person is on.
            Key::Named(NamedKey::Escape) if self.open.is_some() => self.close_open(),
            // Escape puts a selection away, as it puts away what a window opened over the
            // list: what was selected stays where it is.
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

    /// Typing in the focused list: move to the first row whose name begins
    /// with what was typed. A list whose model names no rows lets the key go.
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
            // Typing moves to a row; it never extends a selection, whatever
            // case the letters were typed in.
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

    /// [`Self::handle_scrollbar_event`] for the bar laid over the rows.
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
    /// Short path for an owned collection. The row delegate receives the
    /// typed item plus its current display index and stable key.
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

    /// Enable ordinary before/after reordering for the owned vector model.
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

impl<M: ListModel> crate::widget::PaintedRowLifts for VirtualList<M> {
    fn painted_row_lifts(&self) -> Vec<crate::widget::RowLift> {
        self.painted_lifts.clone()
    }
}

impl<M: ListModel + 'static> Widget for VirtualList<M> {
    crate::provides!(
        dyn crate::widget::controls::ActionControls,
        dyn crate::resources::PublishesResources,
        dyn crate::widget::ResourceOperations,
        dyn crate::widget::PaintedRowLifts
    );

    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
        // Every structural rebuild repeats the window's input; what mounts
        // since hears it as it mounts.
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
        // Rows a scroll alone brought near are mounted by the layout pass,
        // which asks for a rebuild the frame's damage describes; anything
        // else reconciles now.
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
        let mut changed = legacy_wakeup || received_changes;
        changed |= self.take_retry();
        // A glide moves only the viewport, which the frame it asks for lays
        // out; rows it brings near mount through that layout's rebuild. It
        // is no change of the model, so it does not report one (as
        // `Scrollable::update`).
        let offset = self.viewport.offset();
        if self.scroll.momentum.is_active() {
            let delta = self.scroll.momentum.advance(now);
            if !self.scroll_by(delta) {
                self.scroll.momentum.stop();
            }
        }
        // A reveal in progress, like a glide, moves only the viewport.
        self.follow_scroll_to(crate::motion::frame_now());
        if self.viewport.offset().to_bits() != offset.to_bits() {
            crate::widget::note_layout_moved();
        }
        if self.reorder.is_active() {
            let motion = self.reorder.tick(self.rect, elapsed, now);
            // A pointer dwelling on a folded branch shows what it holds, where a drop into it
            // will land.
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
        self.disclosure = crate::chrome::disclosure_gutter(theme);
        self.row_line = f64::from(theme.tokens.metrics.row_single);
        // A chevron hangs left of the column so the column never moves: one left edge.
        let gutter = if self.tree.is_some() || self.model.discloses() {
            self.disclosure
        } else {
            0.0
        };
        self.lead = self.bleed.max(gutter);
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
            // Every row is in view: the page around the list scrolls them.
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
        // The tree built here holds no node of an earlier one: the rows let
        // go of are gone from it.
        self.letting_go.clear();
        self.layout_node = Some(node);
        node
    }

    /// Rows a scroll brought near are mounted in place, and those it took
    /// away let go of; the rows themselves carry out their own changes.
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
        // A list too narrow for one thing at a time shows the item in front of it, never grown
        // into the row: the row that stood open closes as the window narrows.
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
        // Rows stand where the layout was built, moved by the scroll since.
        let slack = self.end_slack();
        let child_origin = Point::new(
            self.rect.x0,
            self.rect.y0 + slack + self.layout_offset - self.viewport.offset(),
        );
        // Placed again from an unchanged layout (a scroll, a glide), only
        // the rows near the list's box are placed, and those that were
        // near it before, as they leave; the others measure as they did.
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
        // A placement that found no row near the box placed none, so it
        // narrows nothing: the pointer, its replay and the copy's walk meet
        // every row, as they do after a full layout. A narrowed set left
        // empty would meet no row at all — including the rows this frame
        // mounts, which stand under the pointer before they are placed.
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
            // Placed again, a row keeps the height it was measured at when
            // the layout it stands in was solved.
            if !again {
                measurements.push((row.index, row.key.clone(), row.widget.get_rect().height()));
            }
        }
        // A row measured anew moves the rows after it: a rebuild no damage
        // describes. A row measured for the first time was not on screen
        // yet (a row is measured before it is drawn): the rows a scroll
        // brought near settle at their heights in a rebuild that moves
        // nothing the last frame showed (`ListShown::first_shown`). The
        // width the rows take is known now, and the space around them
        // follows it, which moves every row as a remeasure does.
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
        // Rows that settled their heights moved the end: a list following it stays there, and
        // rows shorter than its box stand at its foot again.
        remeasured |= self.keep_end();
        remeasured |= (self.end_slack() - slack).abs() > 0.5;
        // A list as tall as its rows grows or shrinks with what they measured.
        if self.fit_rows && (self.rect.height() - self.viewport.content_extent()).abs() > 0.5 {
            self.viewport.set_viewport(self.viewport.content_extent());
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
        // A scroll brought other rows near: the rebuild mounting them is
        // described by the frame's damage.
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
        // The rows' menu takes the input while it is up, before the rows behind it do.
        if self.route_menu(event, ctx) {
            return;
        }
        // The hand takes over a reveal in progress from where it is drawn.
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
        // An open rename field takes the keys typed into it; a press outside it commits and
        // falls through to the row.
        if self.route_rename(event, ctx) {
            return;
        }
        // A press on a group's heading is the heading's, before the row under it acts: it
        // opens or closes the group when it is let go there.
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
            // The heading the press landed on takes the keyboard with it, as a row does.
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
        // A left press inside the list is a click waiting on its release, whichever widget the
        // press goes to: a row with a swipe takes the press for the swipe, and a tap it lets go
        // of without moving is still a click on that row. What the press landed on is taken
        // from the list as soon as the finger lifts, whoever answers the release.
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
        if !taking_press && !self.drag_scroll.is_tracking() && !self.reorder.is_tracking() {
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
                // A press on the pill over the foot is the pill's, not a drag of the rows.
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
                        // A press where no row stands, beside a row opened into its item: the
                        // person pressed outside it, and it closes.
                        self.close_open();
                    }
                    *ctx.handled = true;
                } else if let Some((_, key)) = pressed
                    && self.row_at_cursor().is_some_and(|(_, at)| at == key)
                {
                    // The press went to a row's own gesture and came back without one: the
                    // tap's click is the list's.
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
            // Ctrl+scroll zooms (`crate::content_zoom`): the window takes it.
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
                // It moved only this list's view: nothing the app publishes.
                if *ctx.handled {
                    crate::widget::note_view_only_pan();
                }
            }
            // A key a focused row left alone moves through the list, and
            // the list takes the focus from the row.
            WindowEvent::KeyboardInput { event, .. }
                if (self.is_focused || self.mounted.iter().any(|row| row.widget.is_focused()))
                    && event.state == ElementState::Pressed =>
            {
                self.scroll.momentum.stop();
                if event.logical_key == Key::Named(NamedKey::Escape) && self.reorder.cancel() {
                    *ctx.handled = true;
                } else if self.take_menu_key(&event.logical_key, ctx.modifiers.state()) {
                    // The menu it opens takes the keyboard while it is open.
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                } else if let Some(movement) =
                    self.tree_move_chord(&event.logical_key, ctx.modifiers.state())
                {
                    // A tree move the hierarchy has no room for leaves the key alone (a `Tab`
                    // steps the focus on), and one that moves keeps the keyboard on the row.
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
                    // The chord belongs to this list wherever the row can go:
                    // it moves and keeps the focus, and a move it refuses (at
                    // the row's end, or where the order allows none) leaves
                    // the keyboard on that row instead of stepping off it.
                    // A row that takes the keyboard holds it from here, so the
                    // focus stays on that row when whoever owns the order
                    // rebuilds the list.
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

    /// The rows as the retained layout holds them: a row built anew since
    /// stands there as the row it replaces, until the remount mounts it.
    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        if self.letting_go.is_empty() {
            for row in &self.mounted {
                visitor(row.laid_out_widget());
            }
            return true;
        }
        // The rows let go of stand where the layout holds them, among the
        // rows that took their place.
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

    /// The rows near its box: the others are clipped away.
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
        // Each mounted row's node is the list's own, headings standing before
        // the rows: the walk never visits the row widgets, and a row nothing
        // changed about is kept as the node it handed over before.
        child_index < self.mounted.len()
    }

    /// Focus the row an assistive technology moves to, and select it, as the keyboard does.
    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [child] = path else {
            return false;
        };
        // The mounted row whose node that child stands under, headings counted.
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

    /// The row with the keyboard: a menu opened from the keyboard stands
    /// under that row, not under the list.
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

    /// A finger on a row is the row's to claim (a row that swipes takes a sideways drag); the
    /// list scrolls whatever the row leaves to the pointer.
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
        let now = crate::time::now();
        self.overlay_bar
            .as_ref()
            .is_some_and(crate::widget::OverlayScrollbar::needs_animation_frame)
            || self.scroll.momentum.is_active()
            || self
                .scroll
                .momentum
                .give_needs_frame(crate::motion::frame_now())
            || self.reveal_in_progress(crate::motion::frame_now())
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

    /// Rows mounted and let go — those a scroll brought near or took away,
    /// those the model re-keyed, reordered, or whose content was built
    /// anew — are mounted where they stand ([`Self::remount_rows`]) and
    /// draw themselves whole: the list shows them come and go.
    ///
    /// With no layout of its own the list has nothing to mount under yet: the
    /// build that lays it out mounts the rows it shows, so until then it asks
    /// nothing of the host — a list the tree keeps without laying it out (a
    /// closed sidebar) would otherwise ask for a rebuild every frame and
    /// never have one carry it out.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        use crate::widget::StructureChange;
        let mut change = if std::mem::take(&mut self.box_dirty) {
            StructureChange::Rebuild
        } else if (self.structure_dirty || self.remount) && self.layout_node.is_some() {
            // Reported until the rows are mounted, in place where the host
            // can ([`Widget::remount_children`]) or by a rebuild of the tree.
            StructureChange::Children
        } else {
            StructureChange::None
        };
        for row in &mut self.mounted {
            change = change.join(row.widget.take_structure_change());
        }
        change
    }

    /// A scroll that changed nothing else offers the host a copy of the
    /// rows that stay on screen, the bar's track being what does not move
    /// with them; what changed in the rows besides is theirs to say. Any
    /// other change repaints the list.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        // Rows moving after a list change repaint the list every frame; a
        // frame of theirs is never a scroll copy.
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
            // The same rows where they stood: what changed is what the rows
            // repaint (a row built anew for new content paints itself whole),
            // not the list.
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
        self.context_menu_is_open() || self.send_to_alone_open()
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
        // Not drawn this frame, what the screen shows where the list lies
        // is not known to be its rows: the next scroll repaints it.
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

    /// The list and its rows, the rows kept between walks: a walk beside an
    /// unchanged list builds and compares no row ([`Self::accessibility_children`]).
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
