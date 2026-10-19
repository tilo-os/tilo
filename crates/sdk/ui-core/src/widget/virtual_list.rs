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

use taffy::style_helpers::{length, percent};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::presence::PresenceCtx;
use crate::reorder::{DropCandidate, DropPlacement, DropTarget, ReorderController, ReorderDrop};
use crate::selection::KeySelection;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::virtual_viewport::{
    DragScroller, ScrollAlignment, ScrollMomentum, SparseAnchor, SparseVirtualViewport, WheelStep,
};
use crate::widget::scrollbar::{PressOutcome, ScrollMetrics, Scrollbar};
use crate::widget::{Element, TaffyTreeExt, Widget};

const DEFAULT_VIEWPORT_HEIGHT: f64 = 320.0;

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

    /// Current state of a logical row. Paged models can keep stable keys and
    /// geometry while their data moves through loading, ready, and error.
    fn row_state(&self, _index: usize) -> ListRowState<'_> {
        ListRowState::Ready
    }

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
}

/// State of one stable row in a paged or asynchronously populated list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListRowState<'a> {
    Ready,
    Loading,
    Error(&'a str),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DuplicateListKey;

impl fmt::Display for DuplicateListKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("list model keys must be unique")
    }
}

impl std::error::Error for DuplicateListKey {}

/// Convenient fully-resident model. Database and paged sources generally
/// implement [`ListModel`] directly instead.
pub struct VecListModel<T, K> {
    items: Vec<T>,
    keys: Vec<K>,
    index: HashMap<K, usize>,
}

impl<T, K> VecListModel<T, K>
where
    K: Clone + Eq + Hash,
{
    /// Build the key index once.
    ///
    /// # Errors
    ///
    /// Returns [`DuplicateListKey`] when two items resolve to the same key.
    pub fn new(items: Vec<T>, mut key: impl FnMut(&T) -> K) -> Result<Self, DuplicateListKey> {
        let keys = items.iter().map(&mut key).collect::<Vec<_>>();
        let index = keys
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, key)| (key, index))
            .collect::<HashMap<_, _>>();
        if index.len() != keys.len() {
            return Err(DuplicateListKey);
        }
        Ok(Self { items, keys, index })
    }

    #[must_use]
    pub fn item(&self, index: usize) -> Option<&T> {
        self.items.get(index)
    }

    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// # Errors
    ///
    /// Returns [`DuplicateListKey`] when `key` already exists.
    pub fn push(&mut self, key: K, item: T) -> Result<(), DuplicateListKey> {
        if self.index.contains_key(&key) {
            return Err(DuplicateListKey);
        }
        let index = self.items.len();
        self.items.push(item);
        self.keys.push(key.clone());
        self.index.insert(key, index);
        Ok(())
    }

    pub fn remove(&mut self, index: usize) -> Option<T> {
        if index >= self.items.len() {
            return None;
        }
        self.keys.remove(index);
        let item = self.items.remove(index);
        self.rebuild_index();
        Some(item)
    }

    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from >= self.items.len() || to >= self.items.len() || from == to {
            return false;
        }
        let item = self.items.remove(from);
        let key = self.keys.remove(from);
        self.items.insert(to, item);
        self.keys.insert(to, key);
        self.rebuild_index();
        true
    }

    fn rebuild_index(&mut self) {
        self.index.clear();
        self.index.extend(
            self.keys
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, key)| (key, index)),
        );
    }
}

impl<T, K> ListModel for VecListModel<T, K>
where
    K: Clone + Eq + Hash + 'static,
{
    type Key = K;

    fn len(&self) -> usize {
        self.items.len()
    }

    fn key_at(&self, index: usize) -> Option<Self::Key> {
        self.keys.get(index).cloned()
    }

    fn index_of(&self, key: &Self::Key) -> Option<usize> {
        self.index.get(key).copied()
    }
}

type RowBuilder<M> = Box<
    dyn for<'a> FnMut(&M, usize, &<M as ListModel>::Key, ListRowState<'a>, RowSelection) -> Element,
>;
type KeyCallback<K> = Box<dyn FnMut(K)>;
type ContextCallback<K> = Box<dyn FnMut(K, Point, Rect)>;
type SemanticKey<K> = Box<dyn Fn(&K) -> String>;
type SelectCallback<K> = Box<dyn FnMut(K, bool)>;
type ReorderValidator<M> =
    Box<dyn FnMut(&M, &<M as ListModel>::Key, &DropTarget<<M as ListModel>::Key>) -> bool>;
type ReorderCommit<M> = Box<dyn FnMut(&mut M, ReorderDrop<<M as ListModel>::Key>) -> bool>;

/// A row's selection with its neighbours', for a delegate that paints its
/// own selection: a run of selected rows is one lift
/// ([`crate::chrome::RowJoin`]), and a separator hides beside a selected
/// row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RowSelection {
    pub selected: bool,
    /// The row above is selected.
    pub above: bool,
    /// The row below is selected.
    pub below: bool,
    /// A row follows this one.
    pub has_next: bool,
}

impl RowSelection {
    /// The lift's join with its selected neighbours.
    pub fn join(self) -> crate::chrome::RowJoin {
        crate::chrome::RowJoin::new(self.above, self.below)
    }
}

/// A row on its way after a change the person made (`motion::ListChange`):
/// moving from where it stood to its new place, or fading in where it
/// arrived. Rows the change did not move have none.
#[derive(Clone, Copy, Debug)]
struct RowMotion {
    /// How far below its laid-out place it is drawn (negative: above).
    offset: crate::motion::MotionValue<f64>,
    /// Its opacity.
    fade: crate::motion::MotionValue<f64>,
}

impl RowMotion {
    fn rest() -> Self {
        Self {
            offset: crate::motion::MotionValue::new(0.0).with_tolerance(0.25),
            fade: crate::motion::MotionValue::new(1.0),
        }
    }

    fn is_moving(&self, now: Instant) -> bool {
        self.offset.is_moving(now) || self.fade.is_moving(now)
    }

    fn settles_at(&self) -> Option<Instant> {
        self.offset.settles_at().max(self.fade.settles_at())
    }
}

/// Where the mounted rows stood just before a structural change the person
/// made, by key, for the next frame to move them from.
struct PendingListChange<K> {
    before: Vec<(K, f64)>,
    inserts: bool,
    source: crate::tokens::MotionSource,
}

struct MountedRow<K> {
    key: K,
    index: usize,
    state: MountedRowState,
    selection: RowSelection,
    widget: Element,
    node: Option<NodeId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MountedRowState {
    Ready,
    Loading,
    Error(String),
}

impl From<ListRowState<'_>> for MountedRowState {
    fn from(state: ListRowState<'_>) -> Self {
        match state {
            ListRowState::Ready => Self::Ready,
            ListRowState::Loading => Self::Loading,
            ListRowState::Error(message) => Self::Error(message.to_owned()),
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "focus, drag threshold, fetch, structural, and layout invalidation are independent widget states"
)]
pub struct VirtualList<M: ListModel> {
    test_id: Option<String>,
    model: M,
    row_builder: RowBuilder<M>,
    /// The delegate reads [`RowSelection`]: a mounted row is rebuilt when
    /// its own or a neighbour's selection changes.
    selection_aware: bool,
    /// The delegate paints the selection; the list paints only focus.
    delegate_selection: bool,
    viewport: SparseVirtualViewport<M::Key>,
    mounted: Vec<MountedRow<M::Key>>,
    selection: KeySelection<M::Key>,
    multi_select: bool,
    /// Moving the focused row by keyboard (or assistive technology) also
    /// selects it.
    follow_focus: bool,
    focused_key: Option<M::Key>,
    /// What has been typed into the focused list ([`ListModel::label`]).
    typing: crate::type_ahead::TypeAhead,
    on_select: Option<SelectCallback<M::Key>>,
    on_activate: Option<KeyCallback<M::Key>>,
    on_context: Option<ContextCallback<M::Key>>,
    /// The last row a click landed on and when, for a double click.
    last_click: Option<(M::Key, Instant)>,
    semantic_key: Option<SemanticKey<M::Key>>,
    accessible_name: Option<String>,
    rect: Rect,
    last_cursor: Option<Point>,
    is_focused: bool,
    drag_scroll: DragScroller,
    reorder: ReorderController<M::Key>,
    reorder_validator: Option<ReorderValidator<M>>,
    reorder_commit: Option<ReorderCommit<M>>,
    momentum: ScrollMomentum,
    reduce_motion: bool,
    vbar: Scrollbar,
    /// `metrics.scrollbar_gutter` and the row lift's bleed, from the last
    /// layout's theme.
    gutter: f64,
    bleed: f64,
    /// Whether the rows were last laid out beside a scroll bar.
    rows_beside_bar: bool,
    /// The list takes its parent's height, and its viewport follows it.
    fill_height: bool,
    last_tick: Option<Instant>,
    last_anchor: Option<SparseAnchor<M::Key>>,
    requested_range: Option<Range<usize>>,
    request_dirty: bool,
    structure_dirty: bool,
    layout_dirty: bool,
    interaction: crate::interaction::InteractionEnvironment,
    /// The scroll offset the rows' layout was built at: a scroll that brings
    /// no row near moves them in `apply_layout`, without a rebuild.
    layout_offset: f64,
    /// The rows mounted at the last reconcile, and whether a scroll since
    /// brought others near: a rebuild that only mounts and lets go of rows
    /// is one the frame's damage describes
    /// ([`crate::widget::StructureChange::Described`]).
    mounted_range: Range<usize>,
    remount: bool,
    /// What the frame being drawn shows, and what the last frame the host
    /// presented showed: a scroll between the two that changed nothing else
    /// is offered to the host as a copy ([`crate::scroll_copy::offer`]).
    shown_drawing: Option<ListShown>,
    shown_presented: Option<ListShown>,
    /// Rows moving or fading after a change the person made.
    row_motion: HashMap<M::Key, RowMotion>,
    /// A structural change waiting for the next frame to start its motion.
    pending_change: Option<PendingListChange<M::Key>>,
}

/// Where a frame shows the rows: the offset, the list's box, the transform
/// it is drawn with, whether the rows stand beside the bar, and what of the
/// list's own paint (selection, focus, a reorder) the rows do not carry.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ListShown {
    offset: f64,
    rect: Rect,
    transform: Affine,
    beside_bar: bool,
    chrome: u64,
    /// Rows were drawn off their places (a list change moving): the frame
    /// is not a copy of another, whatever else matches.
    in_motion: bool,
}

impl<M: ListModel> VirtualList<M> {
    pub fn new(
        model: M,
        estimated_row_height: f32,
        mut row_builder: impl FnMut(&M, usize, &M::Key) -> Element + 'static,
    ) -> Self {
        Self::new_async(
            model,
            estimated_row_height,
            move |model, index, key, _state| row_builder(model, index, key),
        )
    }

    /// Build a list whose delegate handles ready, loading, and error rows.
    /// The toolkit retains stable-key geometry, requests only the virtualized
    /// window, and rebuilds a mounted delegate when its row state changes.
    pub fn new_async(
        model: M,
        estimated_row_height: f32,
        mut row_builder: impl for<'a> FnMut(&M, usize, &M::Key, ListRowState<'a>) -> Element + 'static,
    ) -> Self {
        Self::build(
            model,
            estimated_row_height,
            Box::new(move |model, index, key, state, _| row_builder(model, index, key, state)),
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
        row_builder: impl for<'a> FnMut(&M, usize, &M::Key, ListRowState<'a>, RowSelection) -> Element
        + 'static,
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
            delegate_selection: false,
            viewport,
            mounted: Vec::new(),
            selection: KeySelection::new(),
            multi_select: false,
            follow_focus: false,
            focused_key: None,
            typing: crate::type_ahead::TypeAhead::default(),
            on_select: None,
            on_activate: None,
            on_context: None,
            last_click: None,
            semantic_key: None,
            accessible_name: None,
            rect: Rect::ZERO,
            last_cursor: None,
            is_focused: false,
            drag_scroll: DragScroller::new(),
            reorder: ReorderController::new(),
            reorder_validator: None,
            reorder_commit: None,
            momentum: ScrollMomentum::new(),
            reduce_motion: false,
            vbar: Scrollbar::vertical(),
            gutter: f64::from(crate::tokens::Metrics::BASE.scrollbar_gutter),
            bleed: f64::from(crate::tokens::Spacing::BASE.s2),
            rows_beside_bar: false,
            fill_height: false,
            last_tick: None,
            last_anchor: None,
            requested_range: None,
            request_dirty: true,
            structure_dirty: true,
            layout_dirty: true,
            interaction: crate::interaction::InteractionEnvironment::default(),
            layout_offset: 0.0,
            mounted_range: 0..0,
            remount: false,
            shown_drawing: None,
            shown_presented: None,
            row_motion: HashMap::new(),
            pending_change: None,
        };
        list.refresh_anchor();
        list
    }

    /// Run `f` with the row's key, the pointer and the row's rect when a row
    /// is right-clicked (or asked for its menu by assistive technology). The
    /// row is selected first, unless it already is.
    #[must_use]
    pub fn on_context(mut self, f: impl FnMut(M::Key, Point, Rect) + 'static) -> Self {
        self.on_context = Some(Box::new(f));
        self
    }

    /// Scroll the row of `key` into view and make it the focused row.
    pub fn reveal(&mut self, key: &M::Key) -> bool {
        let Some(index) = self.model.index_of(key) else {
            return false;
        };
        self.focused_key = Some(key.clone());
        self.viewport
            .scroll_to_index(index, ScrollAlignment::Nearest);
        self.visible_window_changed();
        true
    }

    /// Give this list a stable app-test/inspection id.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.test_id = Some(id.into());
        self
    }

    #[must_use]
    pub fn with_viewport_height(mut self, height: f32) -> Self {
        self.set_viewport_height(height);
        self
    }

    pub fn set_viewport_height(&mut self, height: f32) {
        self.viewport.set_viewport(f64::from(height));
        self.visible_window_changed();
    }

    /// Take the parent's full height (a pane's one list) instead of a fixed
    /// viewport; the viewport follows the laid-out height.
    #[must_use]
    pub fn filling(mut self) -> Self {
        self.fill_height = true;
        self
    }

    #[must_use]
    pub fn multi_select(mut self, enabled: bool) -> Self {
        self.multi_select = enabled;
        self
    }

    /// Select the row the keyboard moves to, as a single-selection list
    /// whose selection is what it shows elsewhere (the open item of a
    /// master-detail window) does. A modifier-extended move still extends.
    #[must_use]
    pub fn selection_follows_focus(mut self) -> Self {
        self.follow_focus = true;
        self
    }

    /// Restore stable-key selection and discard keys absent from this model.
    #[must_use]
    pub fn with_selection(mut self, mut selection: KeySelection<M::Key>) -> Self {
        selection.reconcile(|key| self.model.index_of(key).is_some());
        self.selection = selection;
        self
    }

    #[must_use]
    pub fn with_accessible_name(mut self, name: impl Into<String>) -> Self {
        self.accessible_name = Some(name.into());
        self
    }

    #[must_use]
    pub fn on_select(mut self, callback: impl FnMut(M::Key, bool) + 'static) -> Self {
        self.on_select = Some(Box::new(callback));
        self
    }

    #[must_use]
    pub fn on_activate(mut self, callback: impl FnMut(M::Key) + 'static) -> Self {
        self.on_activate = Some(Box::new(callback));
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

    fn key_for_semantic_target(&self, target: &str) -> Option<M::Key> {
        let encoded = target.strip_prefix("key:")?;
        let encode = self.semantic_key.as_ref()?;
        self.mounted
            .iter()
            .find(|row| encode(&row.key) == encoded)
            .map(|row| row.key.clone())
    }

    /// Enable row reordering. `validate` controls which stable-key targets
    /// receive a preview; `commit` mutates the model after a valid drop and
    /// returns whether the collection changed.
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

    #[must_use]
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

    pub fn update_model<R>(&mut self, update: impl FnOnce(&mut M) -> R) -> R {
        let anchor = self.capture_anchor();
        let result = update(&mut self.model);
        self.mounted.clear();
        self.reconcile_model(anchor.as_ref());
        result
    }

    /// Apply a typed mutation batch to an owned model. Delegates whose stable
    /// keys survive structural edits remain live; only `RowsChanged` keys (or
    /// every key for `Reset`) are rebuilt.
    pub fn update_model_changes<R>(
        &mut self,
        changes: &[ListModelChange<M::Key>],
        update: impl FnOnce(&mut M) -> R,
    ) -> R {
        let anchor = self.capture_anchor();
        self.capture_list_change(changes);
        let result = update(&mut self.model);
        self.apply_model_changes(changes);
        self.reconcile_model(anchor.as_ref());
        result
    }

    fn reconcile_model(&mut self, anchor: Option<&SparseAnchor<M::Key>>) {
        self.viewport
            .reconcile(self.model.len(), |key| self.model.index_of(key), anchor);
        self.selection
            .reconcile(|key| self.model.index_of(key).is_some());
        self.focused_key = self
            .focused_key
            .take()
            .filter(|key| self.model.index_of(key).is_some());
        self.request_dirty = true;
        self.visible_window_changed();
    }

    /// Before a structural change the person just made (an insert, a
    /// removal, a move: `motion::ListChange`), note where the mounted rows
    /// stand, so the next frame moves the survivors from there and fades
    /// arrivals in. A change nobody just asked for (a sync), a reset, a
    /// shortcut's result and reduced motion show at once.
    fn capture_list_change(&mut self, changes: &[ListModelChange<M::Key>]) {
        let structural = changes.iter().any(|change| {
            matches!(
                change,
                ListModelChange::RowsInserted { .. }
                    | ListModelChange::RowsRemoved { .. }
                    | ListModelChange::RowsMoved { .. }
            )
        });
        let resets = changes
            .iter()
            .any(|change| matches!(change, ListModelChange::Reset));
        let source = crate::motion::recent_input(crate::time::now())
            .filter(|source| !source.lands_at_once());
        let (Some(source), true, false, false) = (source, structural, resets, self.reduce_motion)
        else {
            self.pending_change = None;
            return;
        };
        let inserts = changes
            .iter()
            .any(|change| matches!(change, ListModelChange::RowsInserted { .. }));
        if let Some(pending) = self.pending_change.as_mut() {
            // Two changes before a frame: the rows move from where the
            // first found them.
            pending.inserts |= inserts;
            return;
        }
        let before = self
            .mounted
            .iter()
            .filter_map(|row| Some((row.key.clone(), self.viewport.item_view_top(row.index)?)))
            .collect();
        self.pending_change = Some(PendingListChange {
            before,
            inserts,
            source,
        });
    }

    /// Start the motion of a change noted before this frame, and forget
    /// rows that have arrived. A row already moving continues from where it
    /// is drawn.
    fn start_row_motion(&mut self, motion: crate::tokens::EffectiveMotion) {
        use crate::motion::{Direction, ListChange};
        let now = crate::time::now();
        if let Some(change) = self.pending_change.take() {
            let shift = ListChange::MOVE.timing(Direction::Enter, motion, change.source);
            let fade = ListChange::FADE.timing(Direction::Enter, motion, change.source);
            for row in &self.mounted {
                let Some(top) = self.viewport.item_view_top(row.index) else {
                    continue;
                };
                let was = change
                    .before
                    .iter()
                    .find(|(key, _)| *key == row.key)
                    .map(|(_, was)| *was);
                match was {
                    Some(was) if (was - top).abs() > 0.5 => {
                        let entry = self
                            .row_motion
                            .entry(row.key.clone())
                            .or_insert_with(RowMotion::rest);
                        let drawn = entry.offset.get(now);
                        entry.offset.snap(drawn + (was - top));
                        entry.offset.set(0.0, shift, now);
                    }
                    None if change.inserts => {
                        let entry = self
                            .row_motion
                            .entry(row.key.clone())
                            .or_insert_with(RowMotion::rest);
                        entry.fade.snap(0.0);
                        entry.fade.set(1.0, fade, now);
                    }
                    _ => {}
                }
            }
            if let Some(until) = self
                .row_motion
                .values()
                .filter_map(RowMotion::settles_at)
                .max()
            {
                crate::motion::want_frames_until(until);
            }
        }
        self.row_motion.retain(|_, row| row.is_moving(now));
    }

    fn apply_model_changes(&mut self, changes: &[ListModelChange<M::Key>]) {
        if changes
            .iter()
            .any(|change| matches!(change, ListModelChange::Reset))
        {
            self.mounted.clear();
            return;
        }
        let changed_keys = changes
            .iter()
            .filter_map(|change| match change {
                ListModelChange::RowsChanged { keys } => Some(keys.iter()),
                ListModelChange::RowsInserted { .. }
                | ListModelChange::RowsRemoved { .. }
                | ListModelChange::RowsMoved { .. }
                | ListModelChange::Reset => None,
            })
            .flatten()
            .cloned()
            .collect::<HashSet<_>>();
        self.mounted.retain(|row| !changed_keys.contains(&row.key));
    }

    /// Reconcile a source that mutated through shared interior state.
    pub fn notify_model_changed(&mut self) {
        let anchor = self.last_anchor.clone();
        self.reconcile_model(anchor.as_ref());
    }

    /// Reconcile a specific batch from a source mutated through shared
    /// interior state.
    pub fn notify_model_changes(&mut self, changes: &[ListModelChange<M::Key>]) {
        if changes.is_empty() {
            return;
        }
        let anchor = self.last_anchor.clone();
        self.capture_list_change(changes);
        self.apply_model_changes(changes);
        self.reconcile_model(anchor.as_ref());
    }

    pub fn invalidate_rows(&mut self, keys: impl IntoIterator<Item = M::Key>) {
        let keys = keys.into_iter().collect::<HashSet<_>>();
        let before = self.mounted.len();
        self.mounted.retain(|row| !keys.contains(&row.key));
        if self.mounted.len() != before {
            self.structure_dirty = true;
        }
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

    #[must_use]
    pub fn visible_range(&self) -> std::ops::Range<usize> {
        self.viewport.visible_range()
    }

    pub fn set_scroll_offset(&mut self, offset: f64) {
        self.viewport.set_offset(offset);
        // The rows move in `apply_layout`; rows the scroll brought near are
        // mounted by the rebuild the layout pass asks for, which the
        // frame's damage describes.
        self.layout_dirty = true;
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

    /// Where the frame being drawn shows the rows and how far they moved on
    /// screen since the last presented frame, when only a scroll moved them.
    fn scrolled_since_presented(&self) -> Option<(ListShown, f64)> {
        let (drawing, presented) = (self.shown_drawing?, self.shown_presented?);
        (drawing.rect == presented.rect
            && drawing.transform == presented.transform
            && drawing.beside_bar == presented.beside_bar
            && drawing.chrome == presented.chrome
            && drawing.offset.to_bits() != presented.offset.to_bits())
        .then_some((drawing, presented.offset - drawing.offset))
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
        let track = crate::widget::scrollbar::vertical_track(self.rect, self.gutter);
        (track.y1 > track.y0).then_some(track)
    }

    /// The rect a row's lift takes: the list's width, less the bar's gutter
    /// while the rows stand beside the bar.
    fn lift_rect(&self, top: f64, height: f64) -> Rect {
        let x1 = if self.rows_beside_bar {
            self.rect.x1 - self.gutter
        } else {
            self.rect.x1
        };
        Rect::new(
            self.rect.x0,
            self.rect.y0 + top,
            x1,
            self.rect.y0 + top + height,
        )
    }

    /// `index`'s selection and its neighbours'.
    fn row_selection(&self, index: usize) -> RowSelection {
        let selected = |i: Option<usize>| {
            i.and_then(|i| self.model.key_at(i))
                .is_some_and(|key| self.selection.is_selected(&key))
        };
        RowSelection {
            selected: selected(Some(index)),
            above: selected(index.checked_sub(1)),
            below: selected(index.checked_add(1)),
            has_next: index.saturating_add(1) < self.model.len(),
        }
    }

    /// Hand every mounted row its current selection: a `ListRow` takes it
    /// in place, any other selection-aware delegate is rebuilt.
    fn push_row_selection(&mut self) {
        if !self.delegate_selection && !self.selection_aware {
            return;
        }
        let selections: Vec<RowSelection> = self
            .mounted
            .iter()
            .map(|row| self.row_selection(row.index))
            .collect();
        let delegate = self.delegate_selection;
        let mut rebuild = false;
        for (row, selection) in self.mounted.iter_mut().zip(selections) {
            if delegate
                && let Some(list_row) = row
                    .widget
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<crate::widget::ListRow>())
            {
                list_row.set_selected(selection.selected);
                list_row.set_join(selection.join());
                list_row.set_separator(selection.has_next, selection.below);
                row.selection = selection;
            } else if row.selection != selection {
                rebuild = true;
            }
        }
        if rebuild {
            self.reconcile_mounted();
        }
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

    fn capture_anchor(&self) -> Option<SparseAnchor<M::Key>> {
        self.viewport
            .capture_anchor(|index| self.model.key_at(index))
    }

    fn refresh_anchor(&mut self) {
        self.last_anchor = self.capture_anchor();
    }

    fn visible_window_changed(&mut self) {
        self.structure_dirty = true;
        self.layout_dirty = true;
        self.refresh_anchor();
    }

    fn reconcile_mounted(&mut self) {
        let interaction = self.interaction;
        let range = self.viewport.visible_range();
        if self.request_dirty || self.requested_range.as_ref() != Some(&range) {
            self.model.request_range(range.clone());
            self.requested_range = Some(range.clone());
            self.request_dirty = false;
        }
        let old_keys = self
            .mounted
            .iter()
            .map(|row| row.key.clone())
            .collect::<Vec<_>>();
        let mut old = std::mem::take(&mut self.mounted)
            .into_iter()
            .map(|row| (row.key.clone(), row))
            .collect::<HashMap<_, _>>();
        let mut seen = HashSet::new();
        let mut mounted = Vec::with_capacity(range.len());
        let mut replaced = false;
        for index in range {
            let Some(key) = self.model.key_at(index) else {
                continue;
            };
            if !seen.insert(key.clone()) {
                continue;
            }
            let state = self.model.row_state(index);
            let mounted_state = MountedRowState::from(state);
            let selection = if self.selection_aware {
                self.row_selection(index)
            } else {
                RowSelection::default()
            };
            let mut row = old
                .remove(&key)
                .filter(|row| {
                    row.state == mounted_state
                        && (!self.selection_aware
                            || row.selection == selection
                            || row
                                .widget
                                .as_any()
                                .is_some_and(|any| any.is::<crate::widget::ListRow>()))
                })
                .unwrap_or_else(|| {
                    replaced = true;
                    MountedRow {
                        widget: (self.row_builder)(&self.model, index, &key, state, selection),
                        key: key.clone(),
                        index,
                        state: mounted_state,
                        selection,
                        node: None,
                    }
                });
            row.widget.set_interaction_environment(interaction);
            row.index = index;
            mounted.push(row);
        }
        let next_keys = mounted
            .iter()
            .map(|row| row.key.clone())
            .collect::<Vec<_>>();
        if replaced || old_keys != next_keys {
            self.structure_dirty = true;
        }
        self.mounted = mounted;
        self.mounted_range = self.viewport.visible_range();
    }

    fn row_at_cursor(&self) -> Option<(usize, M::Key)> {
        let cursor = self.last_cursor?;
        if !self.rect.contains(cursor) {
            return None;
        }
        let index = self.viewport.hit_test(cursor.y - self.rect.y0)?;
        matches!(self.model.row_state(index), ListRowState::Ready)
            .then(|| self.model.key_at(index).map(|key| (index, key)))
            .flatten()
    }

    fn reorder_candidate(&self, cursor: Point) -> Option<DropCandidate<M::Key>> {
        if self.model.is_empty() || self.rect.height() <= 0.0 {
            return None;
        }
        let local_y = (cursor.y - self.rect.y0).clamp(0.0, self.rect.height() - f64::EPSILON);
        let index = self.viewport.hit_test(local_y)?;
        if !matches!(self.model.row_state(index), ListRowState::Ready) {
            return None;
        }
        let key = self.model.key_at(index)?;
        let top = self.viewport.item_view_top(index)?;
        let height = self.viewport.item_height(index)?.max(1.0);
        let before = local_y < top + height / 2.0;
        let y = self.rect.y0 + if before { top } else { top + height };
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

    fn update_reorder_pointer(&mut self, cursor: Point, now: Instant) {
        let candidate = self.reorder_candidate(cursor);
        let model = &self.model;
        let validator = &mut self.reorder_validator;
        self.reorder
            .update(cursor, candidate, now, |source, target| {
                validator
                    .as_mut()
                    .is_some_and(|validate| validate(model, source, target))
            });
    }

    fn commit_reorder(&mut self, dropped: ReorderDrop<M::Key>) -> bool {
        let anchor = self.capture_anchor();
        let changed = self
            .reorder_commit
            .as_mut()
            .is_some_and(|commit| commit(&mut self.model, dropped));
        if changed {
            self.reconcile_model(anchor.as_ref());
        }
        changed
    }

    fn apply_selection(&mut self, key: M::Key, toggle: bool, extend: bool) {
        if self.multi_select && extend {
            let Some(anchor) = self.selection.anchor().cloned() else {
                self.selection.select_only(key.clone());
                self.emit_selection(key);
                return;
            };
            let (Some(anchor_index), Some(target_index)) =
                (self.model.index_of(&anchor), self.model.index_of(&key))
            else {
                self.selection.select_only(key.clone());
                self.emit_selection(key);
                return;
            };
            let lo = anchor_index.min(target_index);
            let hi = anchor_index.max(target_index);
            let keys = (lo..=hi).filter_map(|index| self.model.key_at(index));
            self.selection.select_range(key.clone(), keys, toggle);
        } else if self.multi_select && toggle {
            self.selection.toggle(key.clone());
        } else {
            self.selection.select_only(key.clone());
        }
        self.emit_selection(key);
    }

    /// A completed click on the row of `key`: select it by the modifiers, and
    /// activate it when it is the second click on the same row in time.
    fn click_row(&mut self, key: M::Key, modifiers: winit::event::Modifiers) {
        let state = modifiers.state();
        let toggle = state.control_key() || state.super_key();
        let extend = state.shift_key();
        self.apply_selection(key.clone(), toggle, extend);
        self.focused_key = Some(key.clone());
        let now = crate::time::now();
        let double = !toggle
            && !extend
            && self.last_click.as_ref().is_some_and(|(last, at)| {
                *last == key
                    && now.saturating_duration_since(*at) < crate::gesture::multi_click_interval()
            });
        if double {
            self.last_click = None;
            if let Some(callback) = &mut self.on_activate {
                callback(key);
            }
        } else {
            self.last_click = Some((key, now));
        }
    }

    /// Open the row's context menu: select it unless it already is, then
    /// hand the host the key, the point and the row's rect.
    fn open_context(&mut self, index: usize, key: M::Key, point: Point) {
        if !self.selection.is_selected(&key) {
            self.apply_selection(key.clone(), false, false);
        }
        self.focused_key = Some(key.clone());
        let rect = match (
            self.viewport.item_view_top(index),
            self.viewport.item_height(index),
        ) {
            (Some(top), Some(height)) => self.lift_rect(top, height),
            _ => self.rect,
        };
        if let Some(callback) = &mut self.on_context {
            callback(key, point, rect);
        }
    }

    fn emit_selection(&mut self, key: M::Key) {
        let selected = self.selection.is_selected(&key);
        if let Some(callback) = &mut self.on_select {
            callback(key, selected);
        }
    }

    fn focus_index(&mut self, index: usize, extend: bool) -> bool {
        let Some(key) = self.model.key_at(index) else {
            return false;
        };
        self.focused_key = Some(key.clone());
        self.select_focused(key, extend);
        self.viewport
            .scroll_to_index(index, ScrollAlignment::Nearest);
        self.visible_window_changed();
        true
    }

    /// The selection after the keyboard moved to `key`: extended to it, or
    /// moved onto it when the selection follows focus.
    fn select_focused(&mut self, key: M::Key, extend: bool) {
        if extend {
            self.apply_selection(key, false, true);
        } else if self.follow_focus && !self.selection.is_selected(&key) {
            self.apply_selection(key, false, false);
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
        self.focused_key = Some(key.clone());
        self.select_focused(key, extend);
        self.visible_window_changed();
        scrolled || index != current
    }

    fn on_key(&mut self, key: &Key, toggle: bool, extend: bool) -> bool {
        let count = self.model.len();
        if count == 0 {
            return false;
        }
        let current = self
            .focused_key
            .as_ref()
            .and_then(|key| self.model.index_of(key));
        match key {
            Key::Named(NamedKey::ArrowDown) => self.focus_index(
                current.map_or(0, |index| {
                    index.saturating_add(1).min(count.saturating_sub(1))
                }),
                extend,
            ),
            Key::Named(NamedKey::ArrowUp) => {
                self.focus_index(current.map_or(0, |index| index.saturating_sub(1)), extend)
            }
            Key::Named(NamedKey::PageDown) => self.page_focus(true, extend),
            Key::Named(NamedKey::PageUp) => self.page_focus(false, extend),
            Key::Named(NamedKey::Home) => self.focus_index(0, extend),
            Key::Named(NamedKey::End) => self.focus_index(count.saturating_sub(1), extend),
            Key::Named(NamedKey::Enter) => self.focused_key.clone().is_some_and(|key| {
                let ready = self.model.index_of(&key).is_some_and(|index| {
                    matches!(self.model.row_state(index), ListRowState::Ready)
                });
                if ready && let Some(callback) = &mut self.on_activate {
                    callback(key);
                }
                ready
            }),
            Key::Character(text)
                if self.multi_select
                    && toggle
                    && (text.as_str() == "a" || text.as_str() == "A") =>
            {
                self.selection.select_all();
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
        let count = self.model.len();
        let labels: Vec<Option<String>> = (0..count).map(|index| self.model.label(index)).collect();
        if labels.iter().all(Option::is_none) {
            return false;
        }
        if text == " " && !self.typing.is_typing(crate::time::now()) {
            return false;
        }
        self.typing.type_text(text, crate::time::now());
        let target = self.typing.target(
            labels.iter().map(|label| label.as_deref().unwrap_or("")),
            current,
        );
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
}

impl<T, K> VirtualList<VecListModel<T, K>>
where
    T: 'static,
    K: Clone + Eq + Hash + 'static,
{
    /// Short path for an owned collection. The row delegate receives the
    /// typed item plus its current display index and stable key.
    ///
    /// # Errors
    ///
    /// Returns [`DuplicateListKey`] when two items resolve to the same key.
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

impl<M: ListModel + 'static> Widget for VirtualList<M> {
    fn set_interaction_environment(
        &mut self,
        environment: crate::interaction::InteractionEnvironment,
    ) {
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
            && self.viewport.visible_range() != self.mounted_range;
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
        if self.momentum.is_active() {
            let delta = self.momentum.advance(now);
            let moved = self.scroll_by(delta);
            changed |= moved;
            if !moved {
                self.momentum.stop();
            }
        }
        if self.reorder.is_active() {
            let motion = self.reorder.tick(self.rect, elapsed, now);
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
            self.momentum.stop();
        }
        self.gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        self.bleed = crate::chrome::row_bleed(theme);
        self.reconcile_mounted();
        // Rows stand one bleed in from the list's edges, so their lift (and
        // a ListRow's) fills the list; beside the bar they leave its gutter.
        self.rows_beside_bar = self.scroll_metrics().overflows();
        let left = px(self.bleed);
        let right = px(self.bleed
            + if self.rows_beside_bar {
                self.gutter
            } else {
                0.0
            });
        let mut child_nodes = Vec::with_capacity(self.mounted.len());
        for row in &mut self.mounted {
            let child = row.widget.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(child).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            style.inset.left = length(left);
            style.inset.right = length(right);
            style.inset.top = length(px(self
                .viewport
                .item_view_top(row.index)
                .unwrap_or_default()));
            style.size.width = Dimension::auto();
            let _ = taffy.set_style(child, style);
            row.node = Some(child);
            child_nodes.push(child);
        }
        let node = taffy.parent_node(
            Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: if self.fill_height {
                        percent(1.0_f32)
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
        self.layout_offset = self.viewport.offset();
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        if self.fill_height
            && self.rect.height() > 0.0
            && (self.rect.height() - self.viewport.viewport()).abs() > 0.5
        {
            self.viewport.set_viewport(self.rect.height());
            self.visible_window_changed();
        }
        // Rows stand where the layout was built, moved by the scroll since.
        let child_origin = Point::new(
            self.rect.x0,
            self.rect.y0 + self.layout_offset - self.viewport.offset(),
        );
        let mut measurements = Vec::with_capacity(self.mounted.len());
        for row in &mut self.mounted {
            let Some(child) = row.node else {
                continue;
            };
            row.widget.apply_layout(taffy, child, child_origin);
            measurements.push((row.index, row.key.clone(), row.widget.get_rect().height()));
        }
        let mut measured = false;
        for (index, key, height) in measurements {
            // A first measurement equal to the estimate moves nothing: the
            // rows a scroll brought near keep the frame described.
            let geometry = |viewport: &SparseVirtualViewport<M::Key>| {
                (
                    viewport.item_height(index).map(f64::to_bits),
                    viewport.offset().to_bits(),
                    viewport.content_extent().to_bits(),
                )
            };
            let before = geometry(&self.viewport);
            self.viewport.measure(index, key, height.max(1.0));
            measured |= geometry(&self.viewport) != before;
        }
        if measured {
            self.visible_window_changed();
        } else {
            self.layout_dirty = false;
            self.refresh_anchor();
        }
        // A scroll brought other rows near: the rebuild mounting them is
        // described by the frame's damage.
        if self.viewport.visible_range() != self.mounted_range {
            self.remount = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.reduce_motion = theme.reduce_motion;
        if self.reduce_motion {
            self.momentum.stop();
        }
        self.start_row_motion(theme.motion());
        self.shown_drawing = Some(ListShown {
            offset: self.viewport.offset(),
            rect: self.rect,
            transform: crate::scroll_copy::on_screen(transform),
            beside_bar: self.rows_beside_bar,
            chrome: self.chrome_fingerprint(),
            in_motion: !self.row_motion.is_empty(),
        });
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        let accent = theme.tokens.colors.intent.accent_ink;
        self.push_row_selection();
        let rects: Vec<Option<Rect>> = self
            .mounted
            .iter()
            .map(|row| {
                let top = self.viewport.item_view_top(row.index)?;
                let height = self.viewport.item_height(row.index)?;
                Some(self.lift_rect(top, height))
            })
            .collect();
        let selections: Vec<RowSelection> = self
            .mounted
            .iter()
            .map(|row| self.row_selection(row.index))
            .collect();
        // A row moving after a change is drawn where it is on its way, and
        // an arriving one at its opacity (`motion::ListChange`).
        let now = crate::time::now();
        let poses: Vec<(f64, f32)> = self
            .mounted
            .iter()
            .map(|row| {
                self.row_motion.get(&row.key).map_or((0.0, 1.0), |motion| {
                    (
                        motion.offset.get(now),
                        crate::motion::unit_opacity(motion.fade.get(now)),
                    )
                })
            })
            .collect();
        for (((row, row_rect), selection), (dy, alpha)) in self
            .mounted
            .iter_mut()
            .zip(rects)
            .zip(selections)
            .zip(poses)
        {
            let Some(row_rect) = row_rect else {
                continue;
            };
            let row_rect = row_rect + vello::kurbo::Vec2::new(0.0, dy);
            let row_transform = transform * Affine::translate((0.0, dy));
            let faded = alpha < 1.0;
            if faded {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    alpha,
                    transform,
                    &row_rect.inflate(4.0, 4.0),
                );
            }
            // The same selection a ListRow paints, so a list of ListRows
            // reads identically to a painted one; a run of selected rows is
            // one lift. A delegate that owns its selection paints its own.
            if !self.delegate_selection {
                crate::chrome::paint_row_selection_joined(
                    scene,
                    transform,
                    row_rect,
                    crate::chrome::SelectionState::active(selection.selected),
                    theme.tokens.colors.surfaces.content,
                    theme,
                    if selection.selected {
                        selection.join()
                    } else {
                        crate::chrome::RowJoin::NONE
                    },
                );
            }
            if self.is_focused && self.focused_key.as_ref() == Some(&row.key) {
                let radius = f64::from(theme.tokens.radii.item);
                crate::widget::paint_focus_ring(
                    scene,
                    transform,
                    row_rect.inset(-2.0),
                    (radius - 2.0).max(0.0),
                    theme,
                );
            }
            row.widget
                .draw(scene, row_transform, text_renderer, theme, presence);
            if faded {
                scene.pop_layer();
            }
        }
        if let Some(preview) = self.reorder.preview() {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                accent,
                None,
                &preview,
            );
        }
        scene.pop_layer();

        if let Some(track) = self.scrollbar_track() {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                crate::widget::scrollbar::track_color(theme),
                None,
                &RoundedRect::from_rect(track, track.width() / 2.0),
            );
            self.vbar
                .draw(scene, transform, track, self.scroll_metrics(), theme, false);
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "keeps child, scrollbar, reorder, scroll, selection, and keyboard precedence explicit"
    )]
    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if self.handle_scrollbar_event(event, ctx.scale_factor) {
            *ctx.handled = true;
            return;
        }
        if !self.drag_scroll.is_tracking() && !self.reorder.is_tracking() {
            crate::widget::route_to_mounted(
                &mut self.mounted,
                |mounted| (&mut mounted.widget, mounted.node),
                event,
                ctx,
            );
        }
        if *ctx.handled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                if self.reorder.is_tracking() {
                    self.momentum.stop();
                    self.update_reorder_pointer(point, crate::time::now());
                    self.last_tick = Some(crate::time::now());
                    *ctx.handled = self.reorder.is_active();
                } else if self.drag_scroll.is_tracking() {
                    let now = crate::time::now();
                    let update = self.drag_scroll.update(point.y, now);
                    self.momentum.stop();
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
                self.momentum.stop();
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if self.reorder.is_tracking() {
                    let source = self.reorder.source().cloned();
                    let active = self.reorder.is_active();
                    if let Some(dropped) = self.reorder.finish() {
                        self.commit_reorder(dropped);
                    } else if !active && let Some(key) = source {
                        self.click_row(key, *ctx.modifiers);
                    }
                    *ctx.handled = true;
                } else if let Some(drag) = self.drag_scroll.finish(crate::time::now()) {
                    if drag.dragged {
                        if self.reduce_motion {
                            self.momentum.stop();
                        } else {
                            self.momentum.fling(drag.velocity);
                            self.last_tick = Some(crate::time::now());
                        }
                    } else if let Some((_, key)) = self.row_at_cursor() {
                        self.click_row(key, *ctx.modifiers);
                    }
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.on_context.is_some() => {
                if let (Some((index, key)), Some(point)) = (self.row_at_cursor(), self.last_cursor)
                {
                    self.open_context(index, key, point);
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, phase, .. }
                if self
                    .last_cursor
                    .is_some_and(|point| self.rect.contains(point)) =>
            {
                self.last_tick = Some(crate::time::now());
                *ctx.handled = match self.momentum.wheel(delta, *phase, self.reduce_motion) {
                    WheelStep::Scroll(delta) => {
                        let moved = self.scroll_by(delta);
                        self.momentum.wheel_applied(delta, moved);
                        moved
                    }
                    WheelStep::Fling => true,
                    WheelStep::Ignored => false,
                };
            }
            // A key a focused row left alone moves through the list, and
            // the list takes the focus from the row.
            WindowEvent::KeyboardInput { event, .. }
                if (self.is_focused || self.mounted.iter().any(|row| row.widget.is_focused()))
                    && event.state == ElementState::Pressed =>
            {
                self.momentum.stop();
                if event.logical_key == Key::Named(NamedKey::Escape) && self.reorder.cancel() {
                    *ctx.handled = true;
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

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.mounted
            .iter_mut()
            .map(|row| row.widget.as_mut() as &mut dyn Widget)
            .collect()
    }

    fn accessibility_child_parent(&self, child_index: usize) -> Option<usize> {
        (child_index < self.mounted.len()).then_some(child_index)
    }

    fn name(&self) -> &'static str {
        "VirtualList"
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

    fn wants_pointer_capture(&self) -> bool {
        self.drag_scroll.is_tracking()
            || self.reorder.is_tracking()
            || self.vbar.is_dragging()
            || self
                .mounted
                .iter()
                .any(|row| row.widget.wants_pointer_capture())
    }

    fn needs_animation_frame(&self) -> bool {
        let now = crate::time::now();
        self.momentum.is_active()
            || self.row_motion.values().any(|row| row.is_moving(now))
            || self.reorder.is_active()
            || self.model.needs_poll()
            || self
                .mounted
                .iter()
                .any(|row| row.widget.needs_animation_frame())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty =
            std::mem::take(&mut self.structure_dirty) | std::mem::take(&mut self.remount);
        for row in &mut self.mounted {
            dirty |= row.widget.take_structure_dirty();
        }
        dirty
    }

    /// Mounting the rows a scroll brought near, and letting go of the ones
    /// it took away, is described: they come and go inside what the list
    /// shows, and a mounted row draws itself whole. Any other change to the
    /// rows (a model change, a rebuilt delegate) is not.
    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        use crate::widget::StructureChange;
        let mut change = if std::mem::take(&mut self.structure_dirty) {
            StructureChange::Rebuild
        } else if std::mem::take(&mut self.remount) {
            StructureChange::Described
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
        let in_motion = self.shown_drawing.is_some_and(|shown| shown.in_motion);
        if !in_motion && let Some((shown, delta)) = self.scrolled_since_presented() {
            let overlays = self.scrollbar_track().into_iter().collect();
            if crate::scroll_copy::offer(
                self,
                shown.rect,
                vello::kurbo::Vec2::new(0.0, delta),
                overlays,
                shown.transform,
            ) {
                let first = regions.len();
                for row in &self.mounted {
                    row.widget.collect_damage(regions);
                }
                if let Some(changed) = regions.get_mut(first..) {
                    for rect in changed {
                        *rect = rect.intersect(shown.rect);
                    }
                }
                return;
            }
        }
        let shown_changed = self.shown_drawing != self.shown_presented;
        if in_motion || shown_changed || self.needs_redraw() {
            regions.push(self.rect);
        }
    }

    fn mark_drawn(&mut self) {
        // Not drawn this frame, what the screen shows where the list lies
        // is not known to be its rows: the next scroll repaints it.
        self.shown_presented = self.shown_drawing.take();
        for row in &mut self.mounted {
            row.widget.mark_drawn();
        }
    }

    fn is_layout_dirty(&self) -> bool {
        self.layout_dirty || self.mounted.iter().any(|row| row.widget.is_layout_dirty())
    }

    fn layout_needs_measure(&self) -> bool {
        self.layout_dirty
            || self
                .mounted
                .iter()
                .any(|row| row.widget.layout_needs_measure())
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        let count = self.model.len();
        let children = self
            .mounted
            .iter()
            .map(|row| {
                let child = row.widget.accessibility();
                let name = child
                    .as_ref()
                    .and_then(|node| node.name.clone())
                    .or_else(|| first_descendant_accessible_name(row.widget.as_ref()));
                let mut item = AccessNode::new(Role::ListItem)
                    .with_optional_name(name)
                    .with_position(row.index.saturating_add(1), count)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .focused(self.is_focused && self.focused_key.as_ref() == Some(&row.key))
                            .selected(self.selection.is_selected(&row.key))
                            .disabled(!self.is_enabled()),
                    );
                match &row.state {
                    MountedRowState::Ready => {}
                    MountedRowState::Loading => item.state.busy = true,
                    MountedRowState::Error(message) => {
                        item.state.invalid = true;
                        item.description = Some(message.clone());
                    }
                }
                if let (Some(top), Some(height)) = (
                    self.viewport.item_view_top(row.index),
                    self.viewport.item_height(row.index),
                ) {
                    // The row's own box: one bleed in from the list's edges.
                    let widget = row.widget.get_rect();
                    item = item.with_bounds(Rect::new(
                        widget.x0,
                        self.rect.y0 + top,
                        widget.x1,
                        self.rect.y0 + top + height,
                    ));
                }
                if let Some(encode) = &self.semantic_key {
                    item = item.with_action_target(format!("key:{}", encode(&row.key)));
                }
                if let Some(child) = child {
                    item = item.with_children(vec![child]);
                }
                item
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_optional_name(
                    self.accessible_name
                        .as_deref()
                        .map(crate::i18n::I18nManager::lookup_plain),
                )
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.is_focused)
                        .disabled(!self.is_enabled()),
                )
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        use crate::a11y::Action;

        let Some(key) = self.key_for_semantic_target(target) else {
            return false;
        };
        let Some(index) = self.model.index_of(&key) else {
            return false;
        };
        match action {
            Action::Focus => {
                self.focused_key = Some(key.clone());
                self.select_focused(key, false);
                self.viewport
                    .scroll_to_index(index, ScrollAlignment::Nearest);
                self.visible_window_changed();
                true
            }
            Action::Activate => {
                if !matches!(self.model.row_state(index), ListRowState::Ready) {
                    return false;
                }
                self.focused_key = Some(key.clone());
                self.apply_selection(key.clone(), false, false);
                if let Some(callback) = &mut self.on_activate {
                    callback(key);
                }
                true
            }
            Action::ScrollIntoView => {
                self.viewport
                    .scroll_to_index(index, ScrollAlignment::Nearest);
                self.visible_window_changed();
                true
            }
            Action::ShowContextMenu if self.on_context.is_some() => {
                let point = match (
                    self.viewport.item_view_top(index),
                    self.viewport.item_height(index),
                ) {
                    (Some(top), Some(height)) => self.lift_rect(top, height).center(),
                    _ => self.rect.center(),
                };
                self.open_context(index, key, point);
                true
            }
            _ => false,
        }
    }
}

fn first_descendant_accessible_name(widget: &dyn Widget) -> Option<String> {
    widget.children().into_iter().find_map(|child| {
        child
            .accessibility()
            .and_then(|node| node.name)
            .or_else(|| first_descendant_accessible_name(child))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Duration;
    use automerge::AutoCommit;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use crate::testing::Dispatch;
    use taffy::AvailableSpace;

    struct HugeList {
        count: usize,
    }

    impl ListModel for HugeList {
        type Key = u64;

        fn len(&self) -> usize {
            self.count
        }

        fn key_at(&self, index: usize) -> Option<Self::Key> {
            (index < self.count)
                .then(|| u64::try_from(index).ok())
                .flatten()
        }

        fn index_of(&self, key: &Self::Key) -> Option<usize> {
            usize::try_from(*key)
                .ok()
                .filter(|index| *index < self.count)
        }
    }

    struct SharedOrder {
        keys: Rc<RefCell<Vec<u64>>>,
    }

    impl ListModel for SharedOrder {
        type Key = u64;

        fn len(&self) -> usize {
            self.keys.borrow().len()
        }

        fn key_at(&self, index: usize) -> Option<Self::Key> {
            self.keys.borrow().get(index).copied()
        }

        fn index_of(&self, key: &Self::Key) -> Option<usize> {
            self.keys
                .borrow()
                .iter()
                .position(|candidate| candidate == key)
        }
    }

    struct NotifyingOrder {
        keys: Rc<RefCell<Vec<u64>>>,
        changes: Rc<RefCell<Vec<ListModelChange<u64>>>>,
    }

    impl ListModel for NotifyingOrder {
        type Key = u64;

        fn len(&self) -> usize {
            self.keys.borrow().len()
        }

        fn key_at(&self, index: usize) -> Option<Self::Key> {
            self.keys.borrow().get(index).copied()
        }

        fn index_of(&self, key: &Self::Key) -> Option<usize> {
            self.keys
                .borrow()
                .iter()
                .position(|candidate| candidate == key)
        }

        fn poll_changes(&mut self) -> Vec<ListModelChange<Self::Key>> {
            std::mem::take(&mut *self.changes.borrow_mut())
        }
    }

    struct PagedList {
        count: usize,
        states: Rc<RefCell<HashMap<usize, u8>>>,
        requests: Rc<RefCell<Vec<Range<usize>>>>,
        changed: Rc<Cell<bool>>,
        pending: Rc<Cell<bool>>,
    }

    impl ListModel for PagedList {
        type Key = u64;

        fn len(&self) -> usize {
            self.count
        }

        fn key_at(&self, index: usize) -> Option<Self::Key> {
            (index < self.count)
                .then(|| u64::try_from(index).ok())
                .flatten()
        }

        fn index_of(&self, key: &Self::Key) -> Option<usize> {
            usize::try_from(*key)
                .ok()
                .filter(|index| *index < self.count)
        }

        fn row_state(&self, index: usize) -> ListRowState<'_> {
            match self.states.borrow().get(&index).copied() {
                Some(1) => ListRowState::Ready,
                Some(2) => ListRowState::Error("Fetch failed"),
                _ => ListRowState::Loading,
            }
        }

        fn request_range(&mut self, range: Range<usize>) {
            self.requests.borrow_mut().push(range);
        }

        fn poll_changed(&mut self) -> bool {
            self.changed.replace(false)
        }

        fn needs_poll(&self) -> bool {
            self.pending.get()
        }
    }

    struct ProbeRow {
        label: String,
        height: f32,
        consumes_press: bool,
        presses: Rc<Cell<usize>>,
        rect: Rect,
    }

    impl ProbeRow {
        fn new(
            label: impl Into<String>,
            height: f32,
            consumes_press: bool,
            presses: Rc<Cell<usize>>,
        ) -> Self {
            Self {
                label: label.into(),
                height,
                consumes_press,
                presses,
                rect: Rect::ZERO,
            }
        }
    }

    impl Widget for ProbeRow {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            node_map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let style = Style {
                size: Size {
                    width: percent(1.0_f32),
                    height: Dimension::length(self.height),
                },
                ..Default::default()
            };
            taffy.widget_leaf(style, node_map, self)
        }

        crate::rect_geometry!();

        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
            if self.consumes_press
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
            "ProbeRow"
        }

        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            Some(crate::a11y::AccessNode::new(crate::a11y::Role::Button).with_name(&self.label))
        }
    }

    fn lay_out<M: ListModel + 'static>(
        list: &mut VirtualList<M>,
        width: f32,
    ) -> (TaffyTree<()>, NodeId) {
        let doc = AutoCommit::new();
        list.sync(&doc, &automerge::ROOT);
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let node = list.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: AvailableSpace::Definite(width),
                    height: AvailableSpace::Definite(px(list.viewport.viewport())),
                },
            )
            .unwrap();
        list.apply_layout(&taffy, node, Point::ZERO);
        (taffy, node)
    }

    fn cursor(y: f64) -> WindowEvent {
        crate::synthetic_input::cursor_moved(20.0, y)
    }

    fn left(state: ElementState) -> WindowEvent {
        crate::synthetic_input::mouse_input(state, MouseButton::Left)
    }

    fn dispatch<M: ListModel + 'static>(
        list: &mut VirtualList<M>,
        event: &WindowEvent,
        taffy: &TaffyTree<()>,
        node: NodeId,
    ) -> bool {
        Dispatch::new()
            .layout(taffy, node)
            .send(list, event)
            .handled
    }

    #[test]
    fn list_models_keep_stable_unique_keys_through_mutation_and_restore() {
        // Case `vec_model_rejects_duplicate_keys`.
        {
            let result = VecListModel::new(vec![1, 1], |value| *value);
            assert!(matches!(result, Err(DuplicateListKey)));
        }

        // Case `restored_selection_discards_keys_absent_from_the_model`.
        {
            let mut selection = KeySelection::new();
            selection.toggle(1_u64);
            selection.toggle(99_u64);
            let list = VirtualList::new(HugeList { count: 10 }, 24.0, |_, index, _| {
                Box::new(crate::widget::Text::new(&format!("Item {index}")))
            })
            .with_selection(selection);

            assert!(list.selection().is_selected(&1));
            assert!(!list.selection().is_selected(&99));
            assert_eq!(list.selection().len(10), 1);
        }

        // Case `owned_model_mutations_keep_key_index_consistent`.
        {
            let mut model = VecListModel::new(vec!["a", "b", "c"], |value| *value).unwrap();
            assert!(model.move_item(0, 2));
            assert_eq!(model.key_at(2), Some("a"));
            assert_eq!(model.index_of(&"b"), Some(0));
            assert_eq!(model.remove(1), Some("c"));
            assert_eq!(model.index_of(&"a"), Some(1));
            assert!(model.push("d", "d").is_ok());
            assert_eq!(model.index_of(&"d"), Some(2));
            assert_eq!(model.push("d", "duplicate"), Err(DuplicateListKey));
        }

        // Case `from_items_is_a_typed_one_call_list_constructor`.
        {
            let list = VirtualList::from_items(
                vec![(7_u64, "seven")],
                24.0,
                |item| item.0,
                |item, _, _| Box::new(ProbeRow::new(item.1, 24.0, false, Rc::new(Cell::new(0)))),
            )
            .unwrap()
            .with_id("contacts-list");
            assert_eq!(list.model().key_at(0), Some(7));
            assert_eq!(Widget::test_id(&list), Some("contacts-list"));
        }

        // Case `sync_retains_built_row_nodes_and_invalidates_same_key_replacements`.
        {
            let states = Rc::new(RefCell::new(HashMap::new()));
            let model = PagedList {
                count: 1,
                states: states.clone(),
                requests: Rc::default(),
                changed: Rc::default(),
                pending: Rc::default(),
            };
            let mut list = VirtualList::new(model, 24.0, |_, _, key| {
                Box::new(crate::widget::Button::new(&key.to_string(), "Row"))
            })
            .with_viewport_height(48.0);
            let _ = lay_out(&mut list, 300.0);
            list.take_structure_dirty();
            let before = list.mounted[0].node;
            assert!(before.is_some());
            let doc = AutoCommit::new();
            list.sync(&doc, &automerge::ROOT);
            assert_eq!(
                list.mounted[0].node, before,
                "sync preserves a retained row's layout node"
            );
            list.take_structure_dirty();
            states.borrow_mut().insert(0, 1);
            list.sync(&doc, &automerge::ROOT);
            assert!(list.mounted[0].node.is_none());
            assert!(
                list.take_structure_dirty(),
                "same key with new state replaces a delegate and invalidates topology"
            );
        }
    }

    #[test]
    fn semantic_rows_share_scrolled_variable_geometry_and_stable_key_actions() {
        // Case `semantic_row_actions_use_encoded_stable_keys`.
        {
            let activated = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&activated);
            let model = VecListModel::new(vec![10_u64, 20], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 24.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    24.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0)
            .with_semantic_key(u64::to_string)
            .on_activate(move |key| captured.borrow_mut().push(key));
            let _ = lay_out(&mut list, 300.0);

            let semantics = Widget::accessibility(&list).expect("list semantics");
            assert_eq!(
                semantics.children[1].action_target.as_deref(),
                Some("key:20")
            );
            assert!(Widget::perform_semantic_action(
                &mut list,
                "key:20",
                &crate::a11y::Action::Focus,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert_eq!(list.focused_key, Some(20));
            assert!(Widget::perform_semantic_action(
                &mut list,
                "key:20",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert!(list.selection().is_selected(&20));
            assert_eq!(&*activated.borrow(), &[20]);
        }

        // Case `semantic_rows_use_the_same_scrolled_variable_height_geometry_as_layout`.
        {
            let model = VecListModel::new(vec![1_u64, 2, 3], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 24.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    24.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(48.0);
            list.set_scroll_offset(10.0);
            let _ = lay_out(&mut list, 300.0);

            let semantics = Widget::accessibility(&list).expect("list semantics");
            let first = semantics.children.first().expect("first mounted row");
            // One bleed in from the list's edges, and clear of the bar's
            // gutter while the rows overflow.
            let gutter = f64::from(Theme::dark().tokens.metrics.scrollbar_gutter);
            assert_eq!(
                first.bounds.expect("row bounds").to_rect(),
                Rect::new(8.0, -10.0, 300.0 - gutter - 8.0, 14.0)
            );
            assert_eq!(
                list.mounted[0].widget.get_rect(),
                first.bounds.expect("row bounds").to_rect(),
                "semantic and retained delegate geometry agree",
            );
        }

        // Case `page_navigation_scrolls_inside_a_tall_row_without_changing_its_key`.
        {
            let model = VecListModel::new(vec![1_u64, 2, 3], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 24.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    24.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0);
            assert!(list.set_row_height(0, 300.0));
            list.focused_key = Some(1);

            assert!(list.on_key(&Key::Named(NamedKey::PageDown), false, false));
            assert_eq!(list.focused_key, Some(1));
            assert_eq!(list.scroll_offset(), 90.0);
            assert!(list.on_key(&Key::Named(NamedKey::PageUp), false, false));
            assert_eq!(list.focused_key, Some(1));
            assert_eq!(list.scroll_offset(), 0.0);
        }
    }

    #[test]
    fn hundred_million_row_lists_mount_and_request_only_the_window() {
        // Case `hundred_million_rows_mount_only_the_visible_widget_window`.
        {
            let builds = Rc::new(Cell::new(0_usize));
            let captured = Rc::clone(&builds);
            let presses = Rc::new(Cell::new(0));
            let mut list =
                VirtualList::new(HugeList { count: 100_000_000 }, 20.0, move |_, _, key| {
                    captured.set(captured.get().saturating_add(1));
                    Box::new(ProbeRow::new(
                        format!("row {key}"),
                        20.0,
                        false,
                        Rc::clone(&presses),
                    ))
                })
                .with_viewport_height(100.0);
            list.set_scroll_offset(50_000_000.0 * 20.0);
            let _ = lay_out(&mut list, 300.0);

            assert_eq!(list.model().len(), 100_000_000);
            assert!(list.mounted_len() <= 10);
            assert_eq!(builds.get(), list.mounted_len());
            assert!(list.visible_range().len() <= 10);
            let top = list.scrollbar_thumb().expect("overflow has a scrollbar");
            list.set_scroll_offset(list.max_scroll_offset());
            let bottom = list.scrollbar_thumb().expect("scrollbar remains visible");
            assert!(bottom.y0 > top.y0);
            let track = crate::widget::scrollbar::vertical_track(list.rect, list.gutter);
            assert!((bottom.y1 - track.y1).abs() < 0.001);
        }

        // Case `paged_hundred_million_row_list_requests_only_the_window_and_wakes_on_state_changes`.
        {
            let states = Rc::new(RefCell::new(HashMap::new()));
            let requests = Rc::new(RefCell::new(Vec::new()));
            let changed = Rc::new(Cell::new(false));
            let pending = Rc::new(Cell::new(true));
            let builds = Rc::new(RefCell::new(Vec::new()));
            let captured_builds = Rc::clone(&builds);
            let model = PagedList {
                count: 100_000_000,
                states: Rc::clone(&states),
                requests: Rc::clone(&requests),
                changed: Rc::clone(&changed),
                pending: Rc::clone(&pending),
            };
            let mut list = VirtualList::new_async(model, 20.0, move |_, _, key, state| {
                let label = match state {
                    ListRowState::Ready => format!("ready {key}"),
                    ListRowState::Loading => format!("loading {key}"),
                    ListRowState::Error(message) => format!("error {key}: {message}"),
                };
                captured_builds.borrow_mut().push(label.clone());
                Box::new(ProbeRow::new(label, 20.0, false, Rc::new(Cell::new(0))))
            })
            .with_viewport_height(40.0)
            .with_semantic_key(u64::to_string);

            let _ = lay_out(&mut list, 300.0);
            assert!(list.mounted_len() <= 6);
            assert!(
                requests.borrow().iter().all(|range| range.len() <= 6),
                "fetch requests stay bounded to the sparse viewport"
            );
            assert_eq!(requests.borrow().len(), 1, "one request per window");
            let _ = lay_out(&mut list, 300.0);
            assert_eq!(
                requests.borrow().len(),
                1,
                "repeat layout does not enqueue the same window again"
            );
            let semantics = Widget::accessibility(&list).unwrap();
            assert!(semantics.children[0].state.busy);
            assert!(!Widget::perform_semantic_action(
                &mut list,
                "key:0",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));
            assert!(Widget::needs_animation_frame(&list));

            states.borrow_mut().insert(0, 1);
            pending.set(false);
            changed.set(true);
            assert!(Widget::update(
                &mut list,
                &AutoCommit::new(),
                &automerge::ROOT
            ));
            let _ = lay_out(&mut list, 300.0);
            assert_eq!(
                requests.borrow().len(),
                2,
                "a model wakeup may refill the same window once"
            );
            let semantics = Widget::accessibility(&list).unwrap();
            assert!(!semantics.children[0].state.busy);
            assert!(!semantics.children[0].state.invalid);
            assert!(builds.borrow().iter().any(|label| label == "ready 0"));
            assert!(Widget::perform_semantic_action(
                &mut list,
                "key:0",
                &crate::a11y::Action::Activate,
                &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
                &automerge::ROOT,
            ));

            states.borrow_mut().insert(1, 2);
            changed.set(true);
            assert!(Widget::update(
                &mut list,
                &AutoCommit::new(),
                &automerge::ROOT
            ));
            let _ = lay_out(&mut list, 300.0);
            let semantics = Widget::accessibility(&list).unwrap();
            assert!(semantics.children[1].state.invalid);
            assert_eq!(
                semantics.children[1].description.as_deref(),
                Some("Fetch failed")
            );
            assert!(!Widget::needs_animation_frame(&list));
        }
    }

    #[test]
    fn delegates_follow_stable_keys_and_rebuild_only_what_changed() {
        // Case `mounted_widgets_follow_stable_keys_across_reorder`.
        {
            let keys = Rc::new(RefCell::new(vec![1, 2, 3, 4]));
            let builds = Rc::new(Cell::new(0_usize));
            let captured = Rc::clone(&builds);
            let mut list = VirtualList::new(
                SharedOrder {
                    keys: Rc::clone(&keys),
                },
                20.0,
                move |_, _, key| {
                    captured.set(captured.get().saturating_add(1));
                    Box::new(ProbeRow::new(
                        key.to_string(),
                        20.0,
                        false,
                        Rc::new(Cell::new(0)),
                    ))
                },
            )
            .with_viewport_height(80.0);
            let _ = lay_out(&mut list, 300.0);
            assert_eq!(builds.get(), 4);

            keys.borrow_mut().swap(0, 3);
            list.notify_model_changed();
            let _ = lay_out(&mut list, 300.0);
            assert_eq!(builds.get(), 4, "visible keyed widgets are retained");
        }

        // Case `granular_async_changes_rebuild_only_named_delegates`.
        {
            let keys = Rc::new(RefCell::new(vec![1, 2, 3, 4]));
            let changes = Rc::new(RefCell::new(Vec::new()));
            let builds = Rc::new(RefCell::new(Vec::new()));
            let captured = Rc::clone(&builds);
            let mut list = VirtualList::new(
                NotifyingOrder {
                    keys,
                    changes: Rc::clone(&changes),
                },
                20.0,
                move |_, _, key| {
                    captured.borrow_mut().push(*key);
                    Box::new(ProbeRow::new(
                        key.to_string(),
                        20.0,
                        false,
                        Rc::new(Cell::new(0)),
                    ))
                },
            )
            .with_viewport_height(80.0);
            let _ = lay_out(&mut list, 300.0);
            assert_eq!(*builds.borrow(), vec![1, 2, 3, 4]);

            changes
                .borrow_mut()
                .push(ListModelChange::RowsChanged { keys: vec![2] });
            assert!(Widget::update(
                &mut list,
                &AutoCommit::new(),
                &automerge::ROOT,
            ));
            let _ = lay_out(&mut list, 300.0);

            assert_eq!(*builds.borrow(), vec![1, 2, 3, 4, 2]);
            assert_eq!(list.mounted_len(), 4);
        }

        // Case `granular_owned_structural_batch_retains_live_delegates`.
        {
            let builds = Rc::new(Cell::new(0_usize));
            let captured = Rc::clone(&builds);
            let model = VecListModel::new(vec![1_u64, 2, 3, 4], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 20.0, move |_, _, key| {
                captured.set(captured.get().saturating_add(1));
                Box::new(ProbeRow::new(
                    key.to_string(),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0);
            let _ = lay_out(&mut list, 300.0);
            assert_eq!(builds.get(), 4);

            list.update_model_changes(
                &[ListModelChange::RowsInserted { index: 4, count: 1 }],
                |model| model.push(5, 5).unwrap(),
            );
            let _ = lay_out(&mut list, 300.0);
            assert_eq!(builds.get(), 5, "only the inserted key mounts");

            list.update_model_changes(
                &[ListModelChange::RowsMoved {
                    from: 4,
                    to: 0,
                    count: 1,
                }],
                |model| assert!(model.move_item(4, 0)),
            );
            let _ = lay_out(&mut list, 300.0);
            assert_eq!(builds.get(), 5, "moving stable keys retains delegates");
        }

        // Case `insertion_above_viewport_preserves_the_stable_key_anchor`.
        {
            let keys = Rc::new(RefCell::new((0_u64..100).collect::<Vec<_>>()));
            let mut list = VirtualList::new(
                SharedOrder {
                    keys: Rc::clone(&keys),
                },
                20.0,
                |_, _, key| {
                    Box::new(ProbeRow::new(
                        key.to_string(),
                        20.0,
                        false,
                        Rc::new(Cell::new(0)),
                    ))
                },
            )
            .with_viewport_height(40.0);
            list.set_scroll_offset(50.0 * 20.0 + 5.0);
            let before = list.viewport.item_view_top(50);

            keys.borrow_mut().insert(0, 1_000);
            list.notify_model_changed();

            assert_eq!(list.model().index_of(&50), Some(51));
            assert_eq!(list.viewport.item_view_top(51), before);
        }

        // Case `delegated_row_layout_feeds_variable_height_geometry`.
        {
            let model = VecListModel::new(vec![0_u64, 1, 2], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 20.0, |_, _, key| {
                let height = if *key == 1 { 55.0 } else { 20.0 };
                Box::new(ProbeRow::new(
                    key.to_string(),
                    height,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0);
            let _ = lay_out(&mut list, 200.0);

            assert_eq!(list.viewport.item_height(0), Some(20.0));
            assert_eq!(list.viewport.item_height(1), Some(55.0));
            assert_eq!(list.viewport.item_top(2), Some(75.0));
        }
    }

    #[test]
    fn pointer_input_separates_delegate_presses_drag_scroll_and_selection() {
        // Case `interactive_delegate_consumes_press_without_selecting_its_row`.
        {
            let presses = Rc::new(Cell::new(0));
            let captured = Rc::clone(&presses);
            let model = VecListModel::new(vec![7_u64], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 30.0, move |_, _, key| {
                Box::new(ProbeRow::new(
                    format!("action {key}"),
                    30.0,
                    true,
                    Rc::clone(&captured),
                ))
            })
            .with_viewport_height(30.0);
            let (taffy, node) = lay_out(&mut list, 200.0);

            dispatch(&mut list, &cursor(15.0), &taffy, node);
            assert!(dispatch(
                &mut list,
                &left(ElementState::Pressed),
                &taffy,
                node
            ));
            assert_eq!(presses.get(), 1);
            assert!(!list.selection().is_selected(&7));

            let semantics = list.accessibility().unwrap();
            assert_eq!(semantics.role, crate::a11y::Role::List);
            assert_eq!(semantics.children[0].role, crate::a11y::Role::ListItem);
            assert_eq!(semantics.children[0].name.as_deref(), Some("action 7"));
            assert_eq!(
                semantics.children[0].children[0].role,
                crate::a11y::Role::Button
            );
        }

        // Case `background_drag_scrolls_but_click_selects`.
        {
            let model = VecListModel::new((0_u64..100).collect(), |key| *key).unwrap();
            let mut list = VirtualList::new(model, 20.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0);
            let (taffy, node) = lay_out(&mut list, 200.0);

            dispatch(&mut list, &cursor(50.0), &taffy, node);
            dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
            dispatch(&mut list, &cursor(10.0), &taffy, node);
            dispatch(&mut list, &left(ElementState::Released), &taffy, node);
            assert_eq!(list.scroll_offset(), 40.0);
            assert!(list.selection().is_empty(100));

            list.set_scroll_offset(0.0);
            let (taffy, node) = lay_out(&mut list, 200.0);
            dispatch(&mut list, &cursor(10.0), &taffy, node);
            dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
            dispatch(&mut list, &left(ElementState::Released), &taffy, node);
            assert!(list.selection().is_selected(&0));
        }

        // Case `reduced_motion_keeps_direct_scroll_without_wheel_or_drag_decay`.
        {
            let model = VecListModel::new((0_u64..100).collect(), |key| *key).unwrap();
            let mut list = VirtualList::new(model, 20.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(100.0);
            let (taffy, node) = lay_out(&mut list, 200.0);
            list.reduce_motion = true;

            dispatch(&mut list, &cursor(50.0), &taffy, node);
            let wheel = crate::synthetic_input::mouse_wheel(0.0, -1.0);
            assert!(dispatch(&mut list, &wheel, &taffy, node));
            assert_eq!(list.scroll_offset(), 60.0);
            assert!(!list.momentum.is_active());

            let now = crate::time::now();
            let started = now.checked_sub(Duration::from_millis(20)).unwrap_or(now);
            let moved = now.checked_sub(Duration::from_millis(5)).unwrap_or(now);
            list.drag_scroll.begin(50.0, list.scroll_offset(), started);
            assert!(list.drag_scroll.update(10.0, moved).is_some());
            assert!(dispatch(
                &mut list,
                &left(ElementState::Released),
                &taffy,
                node
            ));
            assert!(!list.momentum.is_active());
            assert!(!list.needs_animation_frame());
        }
    }

    #[test]
    fn list_reorder_previews_and_commits_valid_drops_only() {
        // Case `owned_list_reorder_previews_and_commits_by_stable_key`.
        {
            let mut list = VirtualList::from_items(
                vec![0_u64, 1, 2],
                20.0,
                |key| *key,
                |key, _, _| {
                    Box::new(ProbeRow::new(
                        key.to_string(),
                        20.0,
                        false,
                        Rc::new(Cell::new(0)),
                    ))
                },
            )
            .unwrap()
            .with_viewport_height(60.0)
            .reorderable();
            let (taffy, node) = lay_out(&mut list, 200.0);

            dispatch(&mut list, &cursor(10.0), &taffy, node);
            dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
            dispatch(&mut list, &cursor(55.0), &taffy, node);
            assert_eq!(
                list.reorder_target(),
                Some(&DropTarget::new(2, DropPlacement::After))
            );
            assert!(list.reorder_preview().is_some());
            dispatch(&mut list, &left(ElementState::Released), &taffy, node);

            assert_eq!(list.model().items(), &[1, 2, 0]);
            assert_eq!(list.model().index_of(&0), Some(2));
            assert_eq!(list.reorder_target(), None);
        }

        // Case `invalid_list_drop_has_no_preview_and_does_not_mutate`.
        {
            let model = VecListModel::new(vec![0_u64, 1], |key| *key).unwrap();
            let mut list = VirtualList::new(model, 20.0, |_, _, key| {
                Box::new(ProbeRow::new(
                    key.to_string(),
                    20.0,
                    false,
                    Rc::new(Cell::new(0)),
                ))
            })
            .with_viewport_height(40.0)
            .with_reorder(|_, _, _| false, |_, _| true);
            let (taffy, node) = lay_out(&mut list, 200.0);

            dispatch(&mut list, &cursor(10.0), &taffy, node);
            dispatch(&mut list, &left(ElementState::Pressed), &taffy, node);
            dispatch(&mut list, &cursor(35.0), &taffy, node);
            assert!(list.reorder_preview().is_none());
            dispatch(&mut list, &left(ElementState::Released), &taffy, node);
            assert_eq!(list.model().items(), &[0, 1]);
        }
    }

    fn plain_rows(count: u32) -> VirtualList<VecListModel<u32, u32>> {
        VirtualList::from_items(
            (0..count).collect(),
            40.0,
            |item| *item,
            |item, _, _| Box::new(crate::widget::Text::new(&format!("row {item}"))) as Element,
        )
        .unwrap()
        .with_viewport_height(200.0)
    }

    #[test]
    fn rows_stand_a_bleed_in_and_the_bar_takes_the_shared_gutter() {
        let mut list = plain_rows(100);
        let _ = lay_out(&mut list, 300.0);
        let theme = Theme::dark();
        let bleed = crate::chrome::row_bleed(&theme);
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let row = list.mounted[0].widget.get_rect();
        assert!((row.x0 - bleed).abs() < 0.5, "a bleed in: {row:?}");
        assert!(
            (row.x1 - (300.0 - gutter - bleed)).abs() < 0.5,
            "clear of the bar: {row:?}"
        );
        let track = list.scrollbar_track().unwrap();
        assert!(track.x0 >= 300.0 - gutter && track.x1 <= 300.0);
        // Without overflow the rows take the list's width less the bleed.
        let mut short = plain_rows(2);
        let _ = lay_out(&mut short, 300.0);
        let row = short.mounted[0].widget.get_rect();
        assert!((row.x1 - (300.0 - bleed)).abs() < 0.5);
        assert!(short.scrollbar_track().is_none());
    }

    #[test]
    fn a_filling_list_takes_its_parents_height_as_its_viewport() {
        let mut list = plain_rows(1000).filling();
        let doc = AutoCommit::new();
        let mut taffy = TaffyTree::new();
        let mut node_map = HashMap::new();
        let mut lay_out_in = |list: &mut VirtualList<_>, taffy: &mut TaffyTree<()>| {
            list.sync(&doc, &automerge::ROOT);
            taffy.clear();
            node_map.clear();
            let node = list.build_layout(taffy, &Theme::dark(), &mut node_map);
            let parent = taffy
                .new_with_children(
                    Style {
                        size: Size::from_lengths(300.0_f32, 600.0_f32),
                        ..Default::default()
                    },
                    &[node],
                )
                .unwrap();
            taffy
                .compute_layout(
                    parent,
                    Size {
                        width: AvailableSpace::MaxContent,
                        height: AvailableSpace::MaxContent,
                    },
                )
                .unwrap();
            list.apply_layout(taffy, node, Point::ZERO);
        };
        lay_out_in(&mut list, &mut taffy);
        assert!((list.get_rect().height() - 600.0).abs() < 0.5);
        lay_out_in(&mut list, &mut taffy);
        let rows = list.visible_range();
        assert!(
            rows.end * 40 >= 600,
            "the window covers the height: {rows:?}"
        );
    }

    #[test]
    fn selection_can_follow_the_keyboard_focus() {
        let selected = Rc::new(RefCell::new(Vec::new()));
        let record = Rc::clone(&selected);
        let mut list = plain_rows(10)
            .selection_follows_focus()
            .on_select(move |key, on| record.borrow_mut().push((key, on)));
        let (taffy, node) = lay_out(&mut list, 300.0);
        list.set_focus(true);
        assert!(list.reveal(&2));
        let down = crate::synthetic_input::named_key(NamedKey::ArrowDown);
        assert!(dispatch(&mut list, &down, &taffy, node));
        assert_eq!(list.focused_key(), Some(&3));
        assert!(
            list.selection().is_selected(&3),
            "the focused row is chosen"
        );
        assert!(!list.selection().is_selected(&2));
        assert_eq!(selected.borrow().last(), Some(&(3, true)));
    }

    #[test]
    fn reveal_scrolls_a_row_into_view_and_focuses_it() {
        let mut list = plain_rows(1000);
        let _ = lay_out(&mut list, 300.0);
        assert!(list.reveal(&700));
        let _ = lay_out(&mut list, 300.0);
        assert_eq!(list.focused_key(), Some(&700));
        assert!(list.visible_range().contains(&700));
        assert!(!list.reveal(&5000), "an unknown key reveals nothing");
    }

    #[test]
    fn a_second_click_activates_and_a_right_click_opens_the_rows_menu() {
        let activated = Rc::new(RefCell::new(Vec::new()));
        let menus = Rc::new(RefCell::new(Vec::new()));
        let (a, m) = (Rc::clone(&activated), Rc::clone(&menus));
        let mut list = plain_rows(10)
            .on_activate(move |key| a.borrow_mut().push(key))
            .on_context(move |key, _, rect| m.borrow_mut().push((key, rect)));
        let (taffy, node) = lay_out(&mut list, 300.0);
        let middle = |list: &VirtualList<VecListModel<u32, u32>>, index: usize| {
            list.rect.y0
                + list.viewport.item_view_top(index).unwrap()
                + list.viewport.item_height(index).unwrap() / 2.0
        };
        let row_one = middle(&list, 1);
        let row_three = middle(&list, 3);
        let click = |list: &mut VirtualList<VecListModel<u32, u32>>| {
            dispatch(list, &cursor(row_one), &taffy, node);
            dispatch(list, &left(ElementState::Pressed), &taffy, node);
            dispatch(list, &left(ElementState::Released), &taffy, node);
        };
        click(&mut list);
        assert!(activated.borrow().is_empty(), "one click selects");
        assert!(list.selection().is_selected(&1));
        click(&mut list);
        assert_eq!(*activated.borrow(), [1], "the second click activates");
        dispatch(&mut list, &cursor(row_three), &taffy, node);
        assert!(dispatch(
            &mut list,
            &crate::synthetic_input::mouse_input(ElementState::Pressed, MouseButton::Right),
            &taffy,
            node,
        ));
        let menus = menus.borrow();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].0, 3);
        assert!(
            list.selection().is_selected(&3),
            "the row is selected first"
        );
        assert!(menus[0].1.contains(Point::new(20.0, row_three)));
    }

    #[test]
    fn a_selection_delegate_receives_its_neighbours_and_list_rows_join() {
        let model = VecListModel::new((0..6_u32).collect(), |i| *i).unwrap();
        let mut list = VirtualList::with_selection_delegate(model, 40.0, |_, _, key, _, _| {
            Box::new(crate::widget::ListRow::new(
                format!("r{key}"),
                format!("Row {key}"),
            )) as Element
        })
        .multi_select(true)
        .with_viewport_height(400.0);
        let _ = lay_out(&mut list, 300.0);
        list.selection_mut().select_only(1);
        list.selection_mut().toggle(2);
        list.push_row_selection();
        let joins: Vec<crate::chrome::RowJoin> = list
            .mounted
            .iter_mut()
            .take(4)
            .map(|row| {
                row.widget
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<crate::widget::ListRow>())
                    .unwrap()
                    .join()
            })
            .collect();
        assert_eq!(joins[1], crate::chrome::RowJoin::new(false, true));
        assert_eq!(joins[2], crate::chrome::RowJoin::new(true, false));
        assert!(list.row_selection(0).below);
        assert!(list.row_selection(3).above);
    }

    /// A change the person just made moves the rows it displaced from where
    /// they stood (`motion::ListChange`) and fades a row they added in; a
    /// change nobody just made (a sync) and a shortcut's result land at
    /// once.
    #[test]
    fn rows_the_person_moves_glide_and_a_sync_lands_at_once() {
        use crate::tokens::MotionSource;
        let model = VecListModel::new(vec![1_u64, 2, 3], |key| *key).unwrap();
        let mut list = VirtualList::new(model, 20.0, |_, _, key| {
            Box::new(ProbeRow::new(
                key.to_string(),
                20.0,
                false,
                Rc::new(Cell::new(0)),
            ))
        })
        .with_viewport_height(100.0);
        let _ = lay_out(&mut list, 300.0);
        let motion = Theme::dark().motion();

        // Nobody touched anything: a sync adds a row.
        list.update_model_changes(
            &[ListModelChange::RowsInserted { index: 3, count: 1 }],
            |model| model.push(4, 4).unwrap(),
        );
        let _ = lay_out(&mut list, 300.0);
        list.start_row_motion(motion);
        assert!(list.row_motion.is_empty(), "a sync lands at once");

        // A drag moves the last row to the top: the others move down from
        // where they stood.
        crate::motion::note_source(MotionSource::Pointer);
        list.update_model_changes(
            &[ListModelChange::RowsMoved {
                from: 3,
                to: 0,
                count: 1,
            }],
            |model| assert!(model.move_item(3, 0)),
        );
        let _ = lay_out(&mut list, 300.0);
        list.start_row_motion(motion);
        let now = crate::time::now();
        let first = list.row_motion.get(&1).expect("row 1 moved down");
        assert!(
            first.offset.get(now) < 0.0,
            "drawn where it stood, above its new place"
        );
        assert!(list.needs_animation_frame());
        let later = now + Duration::from_millis(600);
        assert!(
            !list.row_motion.values().any(|row| row.is_moving(later)),
            "and lands"
        );

        // A key moves it back: it lands at once.
        crate::motion::note_source(MotionSource::Keyboard);
        list.update_model_changes(
            &[ListModelChange::RowsMoved {
                from: 0,
                to: 3,
                count: 1,
            }],
            |model| assert!(model.move_item(0, 3)),
        );
        assert!(list.pending_change.is_none(), "a shortcut's result lands");
    }
}

#[cfg(test)]
#[path = "virtual_list_copy_tests.rs"]
mod copy_tests;
