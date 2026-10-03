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
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
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
const BAR_THICKNESS: f64 = 8.0;
const BAR_MARGIN: f64 = 2.0;

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

    pub fn item_mut(&mut self, index: usize) -> Option<&mut T> {
        self.items.get_mut(index)
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

type RowBuilder<M> =
    Box<dyn for<'a> FnMut(&M, usize, &<M as ListModel>::Key, ListRowState<'a>) -> Element>;
type KeyCallback<K> = Box<dyn FnMut(K)>;
type SemanticKey<K> = Box<dyn Fn(&K) -> String>;
type SelectCallback<K> = Box<dyn FnMut(K, bool)>;
type ReorderValidator<M> =
    Box<dyn FnMut(&M, &<M as ListModel>::Key, &DropTarget<<M as ListModel>::Key>) -> bool>;
type ReorderCommit<M> = Box<dyn FnMut(&mut M, ReorderDrop<<M as ListModel>::Key>) -> bool>;

struct MountedRow<K> {
    key: K,
    index: usize,
    state: MountedRowState,
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
    viewport: SparseVirtualViewport<M::Key>,
    mounted: Vec<MountedRow<M::Key>>,
    selection: KeySelection<M::Key>,
    multi_select: bool,
    focused_key: Option<M::Key>,
    on_select: Option<SelectCallback<M::Key>>,
    on_activate: Option<KeyCallback<M::Key>>,
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
    last_tick: Option<Instant>,
    last_anchor: Option<SparseAnchor<M::Key>>,
    requested_range: Option<Range<usize>>,
    request_dirty: bool,
    structure_dirty: bool,
    layout_dirty: bool,
    interaction: crate::interaction::InteractionEnvironment,
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
        row_builder: impl for<'a> FnMut(&M, usize, &M::Key, ListRowState<'a>) -> Element + 'static,
    ) -> Self {
        let mut viewport = SparseVirtualViewport::new(f64::from(estimated_row_height));
        viewport.set_viewport(DEFAULT_VIEWPORT_HEIGHT);
        viewport.set_overscan(viewport.estimated_height() * 2.0);
        viewport.reconcile(model.len(), |key| model.index_of(key), None);
        let mut list = Self {
            test_id: None,
            model,
            row_builder: Box::new(row_builder),
            viewport,
            mounted: Vec::new(),
            selection: KeySelection::new(),
            multi_select: false,
            focused_key: None,
            on_select: None,
            on_activate: None,
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
            last_tick: None,
            last_anchor: None,
            requested_range: None,
            request_dirty: true,
            structure_dirty: true,
            layout_dirty: true,
            interaction: crate::interaction::InteractionEnvironment::default(),
        };
        list.refresh_anchor();
        list
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

    #[must_use]
    pub fn multi_select(mut self, enabled: bool) -> Self {
        self.multi_select = enabled;
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
        let before = self.viewport.visible_range();
        self.viewport.set_offset(offset);
        if self.viewport.visible_range() != before {
            self.structure_dirty = true;
        }
        self.layout_dirty = true;
        self.refresh_anchor();
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
        let x1 = self.rect.x1 - BAR_MARGIN;
        let x0 = x1 - BAR_THICKNESS;
        let y0 = self.rect.y0 + BAR_MARGIN;
        let y1 = self.rect.y1 - BAR_MARGIN;
        (y1 > y0).then_some(Rect::new(x0, y0, x1, y1))
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
            let mut row = old
                .remove(&key)
                .filter(|row| row.state == mounted_state)
                .unwrap_or_else(|| {
                    replaced = true;
                    MountedRow {
                        widget: (self.row_builder)(&self.model, index, &key, state),
                        key: key.clone(),
                        index,
                        state: mounted_state,
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

    fn reconcile_after_reorder(&mut self, anchor: Option<&SparseAnchor<M::Key>>) {
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

    fn commit_reorder(&mut self, dropped: ReorderDrop<M::Key>) -> bool {
        let anchor = self.capture_anchor();
        let changed = self
            .reorder_commit
            .as_mut()
            .is_some_and(|commit| commit(&mut self.model, dropped));
        if changed {
            self.reconcile_after_reorder(anchor.as_ref());
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
        if extend {
            self.apply_selection(key, false, true);
        }
        self.viewport
            .scroll_to_index(index, ScrollAlignment::Nearest);
        self.visible_window_changed();
        true
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
        if extend {
            self.apply_selection(key, false, true);
        }
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
            _ => false,
        }
    }

    fn scroll_by(&mut self, delta: f64) -> bool {
        let before = self.viewport.offset();
        self.set_scroll_offset(before + delta);
        self.viewport.offset().to_bits() != before.to_bits()
    }

    fn route_to_children(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        if let Some(row) = self
            .mounted
            .iter_mut()
            .find(|row| row.widget.wants_pointer_capture())
            && let Some(node) = row.node
        {
            row.widget
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            return;
        }
        for row in self.mounted.iter_mut().rev() {
            let Some(node) = row.node else {
                continue;
            };
            row.widget
                .handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                break;
            }
        }
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

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        self.reconcile_mounted();
        for row in &mut self.mounted {
            row.widget.sync(state, path);
        }
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
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
            let delta = self.momentum.tick(elapsed);
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
        self.reconcile_mounted();
        let mut child_nodes = Vec::with_capacity(self.mounted.len());
        for row in &mut self.mounted {
            let child = row.widget.build_layout(taffy, theme, node_map);
            let mut style = taffy.style(child).cloned().unwrap_or_default();
            style.position = taffy::Position::Absolute;
            style.inset.left = length(0.0_f32);
            style.inset.right = length(0.0_f32);
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
                    height: Dimension::length(px(self.viewport.viewport())),
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
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        let child_origin = Point::new(self.rect.x0, self.rect.y0);
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
            measured |= self.viewport.measure(index, key, height.max(1.0));
        }
        if measured {
            self.visible_window_changed();
        } else {
            self.layout_dirty = false;
            self.refresh_anchor();
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
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            transform,
            &self.rect,
        );
        let accent = theme.tokens.colors.intent.inverted;
        for row in &mut self.mounted {
            let Some(top) = self.viewport.item_view_top(row.index) else {
                continue;
            };
            let Some(height) = self.viewport.item_height(row.index) else {
                continue;
            };
            let row_rect = Rect::new(
                self.rect.x0,
                self.rect.y0 + top,
                self.rect.x1,
                self.rect.y0 + top + height,
            );
            if self.selection.is_selected(&row.key) {
                crate::widget::data_table::paint_selected_row(
                    scene, transform, theme, row_rect, 1.0,
                );
            }
            if self.is_focused && self.focused_key.as_ref() == Some(&row.key) {
                scene.stroke(
                    &Stroke::new(2.0),
                    transform,
                    theme.tokens.colors.border_focus,
                    None,
                    &row_rect,
                );
            }
            row.widget
                .draw(scene, transform, text_renderer, theme, presence);
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
            let radius = BAR_THICKNESS / 2.0;
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                crate::widget::scrollbar::track_color(theme),
                None,
                &RoundedRect::from_rect(track, radius),
            );
            if let Some(thumb) = self.vbar.thumb_rect(track, self.scroll_metrics()) {
                let color = crate::widget::scrollbar::thumb_color(theme, self.vbar.is_active());
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    color,
                    None,
                    &RoundedRect::from_rect(thumb, radius),
                );
            }
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
            self.route_to_children(event, ctx);
        }
        if *ctx.handled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                        let toggle = ctx.modifiers.state().control_key()
                            || ctx.modifiers.state().super_key();
                        self.apply_selection(
                            key.clone(),
                            toggle,
                            ctx.modifiers.state().shift_key(),
                        );
                        self.focused_key = Some(key);
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
                        let toggle = ctx.modifiers.state().control_key()
                            || ctx.modifiers.state().super_key();
                        self.apply_selection(
                            key.clone(),
                            toggle,
                            ctx.modifiers.state().shift_key(),
                        );
                        self.focused_key = Some(key);
                    }
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
            WindowEvent::KeyboardInput { event, .. }
                if self.is_focused && event.state == ElementState::Pressed =>
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
        self.momentum.is_active()
            || self.reorder.is_active()
            || self.model.needs_poll()
            || self
                .mounted
                .iter()
                .any(|row| row.widget.needs_animation_frame())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = std::mem::take(&mut self.structure_dirty);
        for row in &mut self.mounted {
            dirty |= row.widget.take_structure_dirty();
        }
        dirty
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
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.is_focused && self.focused_key.as_ref() == Some(&row.key),
                        selected: self.selection.is_selected(&row.key),
                        disabled: !self.is_enabled(),
                        ..Default::default()
                    });
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
                    item = item.with_bounds(Rect::new(
                        self.rect.x0,
                        self.rect.y0 + top,
                        self.rect.x1,
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
                .with_state(AccessState {
                    focusable: true,
                    focused: self.is_focused,
                    disabled: !self.is_enabled(),
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        _state: &mut AppModel,
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
                self.focused_key = Some(key);
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
    use winit::event::MouseScrollDelta;

    use taffy::AvailableSpace;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, Modifiers};

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

        fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
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

        fn get_rect(&self) -> Rect {
            self.rect
        }

        fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
            Some(crate::a11y::AccessNode::new(crate::a11y::Role::Button).with_name(&self.label))
        }
    }

    fn lay_out<M: ListModel + 'static>(
        list: &mut VirtualList<M>,
        width: f32,
    ) -> (TaffyTree<()>, NodeId) {
        let mut doc = AutoCommit::new();
        list.sync(&mut doc, &automerge::ROOT);
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
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(20.0, y),
        }
    }

    fn left(state: ElementState) -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        }
    }

    fn dispatch<M: ListModel + 'static>(
        list: &mut VirtualList<M>,
        event: &WindowEvent,
        taffy: &TaffyTree<()>,
        node: NodeId,
    ) -> bool {
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus = None;
        list.handle_event(
            event,
            &mut crate::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: &mut doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy,
            },
        );
        handled
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
                Box::new(crate::widget::Button::new(&key.to_string(), "Row", None))
            })
            .with_viewport_height(48.0);
            let _ = lay_out(&mut list, 300.0);
            list.take_structure_dirty();
            let before = list.mounted[0].node;
            assert!(before.is_some());
            let mut doc = AutoCommit::new();
            list.sync(&mut doc, &automerge::ROOT);
            assert_eq!(
                list.mounted[0].node, before,
                "sync preserves a retained row's layout node"
            );
            list.take_structure_dirty();
            states.borrow_mut().insert(0, 1);
            list.sync(&mut doc, &automerge::ROOT);
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
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert_eq!(list.focused_key, Some(20));
            assert!(Widget::perform_semantic_action(
                &mut list,
                "key:20",
                &crate::a11y::Action::Activate,
                &mut AutoCommit::new(),
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
            assert_eq!(
                first.bounds.expect("row bounds").to_rect(),
                Rect::new(0.0, -10.0, 300.0, 14.0)
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
            assert!((bottom.y1 - (list.rect.y1 - BAR_MARGIN)).abs() < 0.001);
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
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));
            assert!(Widget::needs_animation_frame(&list));

            states.borrow_mut().insert(0, 1);
            pending.set(false);
            changed.set(true);
            assert!(Widget::update(
                &mut list,
                &mut AutoCommit::new(),
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
                &mut AutoCommit::new(),
                &automerge::ROOT,
            ));

            states.borrow_mut().insert(1, 2);
            changed.set(true);
            assert!(Widget::update(
                &mut list,
                &mut AutoCommit::new(),
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
                &mut AutoCommit::new(),
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
            let wheel = WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::LineDelta(0.0, -1.0),
                phase: winit::event::TouchPhase::Moved,
            };
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
}
