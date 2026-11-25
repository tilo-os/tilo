//! How a [`super::VirtualList`] takes a change to its model and mounts the rows that change leaves:
//! the rows that stay keep their widgets where they stand, the rows built anew are mounted in their
//! place, and the rows the list lets go of keep the layout walk's place until a remount unmounts
//! them.
use std::collections::{HashMap, HashSet};

use super::{ListKey, VirtualRows};
use super::{MountedRow, MountedRowState, RowBuild, RowSelection, groups};
use crate::collection_transition::ListModelChange;
use crate::virtual_viewport::SparseAnchor;

impl VirtualRows {
    /// Follow a model replaced in place ([`VirtualList::update_model`]), from where the
    /// rows stood before it.
    pub(super) fn model_updated(&mut self, anchor: Option<&SparseAnchor<ListKey>>) {
        self.forget_tree();
        let dropped = std::mem::take(&mut self.mounted);
        self.let_go_of_rows(dropped);
        self.structure_dirty = true;
        self.reconcile_model(anchor);
    }

    /// Follow `changes` to the model ([`VirtualList::update_model_changes`]), from where the
    /// rows stood before them.
    pub(super) fn model_changed(
        &mut self,
        changes: &[ListModelChange<ListKey>],
        anchor: Option<&SparseAnchor<ListKey>>,
    ) {
        self.forget_tree();
        self.apply_model_changes(changes);
        self.reconcile_model(anchor);
    }

    pub(super) fn reconcile_model(&mut self, anchor: Option<&SparseAnchor<ListKey>>) {
        self.regrouped();
        self.close_open_leaving();
        self.viewport
            .reconcile(self.model.len(), |key| self.model.index_of(key), anchor);
        self.selection
            .reconcile(|key| self.model.index_of(key).is_some());
        self.focused_key = self
            .focused_key
            .take()
            .filter(|key| self.model.index_of(key).is_some());
        self.focused_group = self.focused_group.take().filter(|(heading, index)| {
            groups::group_of(&*self.model, *index, self.tail_title.as_deref())
                .is_some_and(|group| group.heading == *heading)
        });
        self.request_dirty = true;
        self.keep_end();
        self.visible_window_changed();
    }

    pub(super) fn capture_list_change(&mut self, changes: &[ListModelChange<ListKey>]) {
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
        // The frame's instant, as everything the frame reads: a change that lands while one is
        // built is made by hand as that frame reads the input before it, not at the clock it is
        // late by.
        let source = crate::collection_transition::RowMotions::<ListKey>::caused_by_hand(
            crate::motion::frame_now(),
            self.reduce_motion,
        )
        .filter(|_| structural && !resets);
        let inserts = changes
            .iter()
            .any(|change| matches!(change, ListModelChange::RowsInserted { .. }));
        let before = self
            .mounted
            .iter()
            .filter_map(|row| Some((row.key.clone(), self.viewport.item_view_top(row.index)?)));
        self.row_motion.capture(before, inserts, source);
    }

    /// Start the glide of a change noted before this frame, at the instant the frame draws it at
    /// ([`crate::motion::frame_now`] inside one): on the clock, a row on its way from an earlier
    /// frame was read past where this frame had mounted it, and a read during layout failed.
    pub(super) fn start_row_motion(&mut self, motion: crate::tokens::EffectiveMotion) {
        let shown = self
            .mounted
            .iter()
            .filter_map(|row| Some((row.key.clone(), self.viewport.item_view_top(row.index)?)));
        self.row_motion
            .start(shown, motion, crate::motion::frame_now());
    }

    fn apply_model_changes(&mut self, changes: &[ListModelChange<ListKey>]) {
        if changes
            .iter()
            .any(|change| matches!(change, ListModelChange::Reset))
        {
            let dropped = std::mem::take(&mut self.mounted);
            self.let_go_of_rows(dropped);
            self.structure_dirty = true;
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
        let (changed, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.mounted)
            .into_iter()
            .partition(|row| changed_keys.contains(&row.key));
        self.let_go_of_rows(changed);
        self.mounted = kept;
    }

    /// The row the keyboard is on, when its widget is let go of or built again: the list holds
    /// it by its key from then on, so a menu opened on it, its ring and the next arrow key stay
    /// with that row however its widget is built anew (a page opening beside the list narrows
    /// every row). The keyboard can reach a row by the window's own focus moves, which the list
    /// does not see until the row's widget goes.
    fn keep_keyboard_row(&mut self, row: &MountedRow<ListKey>) {
        if crate::widget::context_menu_area::focus_within(row.widget.as_ref()) {
            self.focused_group = None;
            self.focused_key = Some(row.key.clone());
        }
    }

    fn let_go_of_rows(&mut self, dropped: Vec<MountedRow<ListKey>>) {
        for row in dropped {
            self.keep_keyboard_row(&row);
            let place = row.index;
            if let Some(laid_out) = row.laid_out() {
                self.letting_go.push((place, laid_out));
            }
        }
    }

    pub fn notify_model_changed(&mut self) {
        let anchor = self.last_anchor.clone();
        self.reconcile_model(anchor.as_ref());
    }

    pub fn notify_model_changes(&mut self, changes: &[ListModelChange<ListKey>]) {
        if changes.is_empty() {
            return;
        }
        let rereading = changes
            .iter()
            .any(|change| matches!(change, ListModelChange::Reset));
        if rereading {
            self.rereading_open = self.open.clone();
        }
        if !rereading
            && self.rereading_open.is_none()
            && changes
                .iter()
                .all(|change| matches!(change, ListModelChange::RowsChanged { .. }))
        {
            let keys = changes
                .iter()
                .filter_map(|change| match change {
                    ListModelChange::RowsChanged { keys } => Some(keys.iter().cloned()),
                    _ => None,
                })
                .flatten()
                .collect::<Vec<_>>();
            self.invalidate_rows(keys);
            return;
        }
        let anchor = self.last_anchor.clone();
        self.capture_list_change(changes);
        self.apply_model_changes(changes);
        self.reconcile_model(anchor.as_ref());
    }

    /// Close the row held open when its item left the list: what the person is reading
    /// where it stands is always a row of the list they see, so an item that goes — to another
    /// place, another query, away — closes with it.
    ///
    /// The app naming its rows again ([`ListModelChange::Reset`]) is not an item going: the
    /// keys go until it answers with the ones it reads now, and the row may stand among them
    /// under the key it had. The open waits for that answer, and closes when the rows arrive
    /// without its row.
    pub(super) fn close_open_leaving(&mut self) {
        let Some(open) = self.open.as_ref() else {
            self.rereading_open = None;
            return;
        };
        if self.model.index_of(open).is_some() {
            self.rereading_open = None;
            return;
        }
        if self.rereading_open.is_some() {
            let named = self
                .model
                .item_presented(open)
                .and_then(|item| self.model.key_presenting(&item));
            if let Some(key) = named {
                if let Some(was) = self.open.replace(key.clone())
                    && was != key
                {
                    self.growing.remove(&was);
                    self.growth.retain(|(grown, _)| *grown != was);
                }
                self.growing.insert(key.clone());
                self.invalidate_rows([key]);
                self.rereading_open = None;
                return;
            }
            if (0..self.model.len()).all(|index| self.model.key_at(index).is_none()) {
                return;
            }
        }
        self.rereading_open = None;
        let open = self.open.take();
        // The row that went takes its growth with it: nothing measures it again, and a
        // height on its way nowhere would ask for frames that never settle.
        if let Some(open) = &open {
            self.growing.remove(open);
            self.growth.retain(|(grown, _)| grown != open);
        }
        self.report_open();
    }

    pub fn invalidate_rows(&mut self, keys: impl IntoIterator<Item = ListKey>) {
        let keys = keys.into_iter().collect::<HashSet<_>>();
        self.regroup_keys(keys.iter());
        let before = self.invalidated.len();
        self.invalidated.extend(
            self.mounted
                .iter()
                .filter(|row| keys.contains(&row.key))
                .map(|row| row.key.clone()),
        );
        if self.invalidated.len() != before {
            self.remount = true;
            self.layout_dirty = true;
        }
    }

    pub(super) fn capture_anchor(&self) -> Option<SparseAnchor<ListKey>> {
        self.viewport
            .capture_anchor(|index| self.model.key_at(index))
    }

    pub(super) fn refresh_anchor(&mut self) {
        self.last_anchor = self.capture_anchor();
    }

    pub(super) fn visible_window_changed(&mut self) {
        self.structure_dirty = true;
        self.layout_dirty = true;
        self.refresh_anchor();
    }

    pub(super) fn reconcile_mounted(&mut self) {
        let interaction = self.interaction;
        let range = self.mount_window();
        let window = range.clone();
        self.shape_tree(&range);
        self.settle_lead();
        let open = self.open.clone();
        if self.request_dirty
            || self.requested_range.as_ref() != Some(&range)
            || self.requested_open != open
        {
            self.model.request_range(range.clone(), open.as_ref());
            self.requested_range = Some(range.clone());
            self.requested_open = open;
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
        self.groups.clear_folded();
        let mut seen = HashSet::new();
        let mut mounted = Vec::with_capacity(range.len());
        let mut replaced = false;
        let now = crate::motion::frame_now();
        for index in range {
            let Some(key) = self.model.key_at(index) else {
                continue;
            };
            if !self.row_open(index) {
                // A closed group: its rows take up nothing and are never built, and its
                // heading's band stands where its first row would (placed with the bands).
                if self.row_starts(index) {
                    self.groups.fold(index);
                    let band = self.groups.height;
                    self.viewport.measure(index, key, band);
                } else {
                    self.viewport.collapse(index, key);
                }
                continue;
            }
            if self.tree.is_some() {
                // Under a folded row: out of the way, never built, as a closed group's are.
                if self.tree_hidden.contains(&key) {
                    self.viewport.collapse(index, key);
                    continue;
                }
                self.viewport.expand(index, &key);
            }
            if !seen.insert(key.clone()) {
                continue;
            }
            let mounted_state = self.observe_row(index, &key, now);
            let selection = if self.selection_aware {
                self.row_selection(index)
            } else {
                RowSelection::default()
            };
            let previous = old.remove(&key);
            let at_that_place = if previous.is_some() {
                None
            } else {
                let at = old
                    .iter()
                    .find(|(_, row)| row.index == index)
                    .map(|(key, _)| key.clone());
                at.and_then(|at| old.remove(&at))
                    .and_then(MountedRow::laid_out)
                    .or_else(|| {
                        let at = self
                            .letting_go
                            .iter()
                            .position(|(place, _)| *place == index)?;
                        Some(self.letting_go.remove(at).1)
                    })
            };
            let keep = previous.as_ref().is_some_and(|row| {
                !self.invalidated.contains(&row.key)
                    && row.state == mounted_state
                    && (!self.selection_aware
                        || row.selection == selection
                        || row
                            .widget
                            .as_any()
                            .is_some_and(|any| any.is::<crate::widget::ListRow>())
                        || (self.delegate_selection
                            && crate::widget::capability::capability::<dyn super::RowSelectionSink>(
                                row.widget.as_ref(),
                            )
                            .is_some()))
            });
            let mut row = match previous {
                Some(mut row) if keep => {
                    if self.delegate_selection
                        && row.selection != selection
                        && let Some(sink) = super::selection_sink(&mut *row.widget)
                    {
                        sink.take_selection(selection);
                        row.selection = selection;
                    }
                    row
                }
                previous => {
                    replaced = true;
                    if let Some(previous) = &previous {
                        self.keep_keyboard_row(previous);
                    }
                    let mut widget = match &mounted_state {
                        MountedRowState::Ready => (self.row_builder)(
                            &*self.model,
                            index,
                            &key,
                            RowBuild {
                                selection,
                                expanded: self.open.as_ref() == Some(&key),
                                nest: crate::num::f32_of_f64(self.disclosure),
                                chevron_column: crate::num::f32_of_f64(self.chevron_column),
                            },
                        ),
                        state => self.arriving_row(index, state),
                    };
                    if self.tree.is_some() {
                        // Every tree's rows read as an outline's, however the app built them:
                        // a row's swipe or hover toolbar wraps the row itself, so the tree's
                        // word reaches through it rather than stopping at the wrapper.
                        outline_row(widget.as_mut(), self.chevron_column);
                    }
                    self.row_builds = self.row_builds.wrapping_add(1);
                    MountedRow {
                        widget,
                        key: key.clone(),
                        index,
                        stamp: self.row_builds,
                        state: mounted_state,
                        selection,
                        node: None,
                        replacing: previous.and_then(MountedRow::laid_out).or(at_that_place),
                    }
                }
            };
            row.widget.set_interaction_environment(interaction);
            row.index = index;
            mounted.push(row);
        }
        let next_keys = mounted
            .iter()
            .map(|row| row.key.clone())
            .collect::<Vec<_>>();
        if old_keys != next_keys || (replaced && self.layout_node.is_none()) {
            self.structure_dirty = true;
        } else if replaced {
            self.remount = true;
            self.layout_dirty = true;
        }
        let mut letting_go = std::mem::take(&mut self.letting_go);
        for (_, row) in old {
            self.keep_keyboard_row(&row);
            let place = row.index;
            if let Some(laid_out) = row.laid_out() {
                letting_go.push((place, laid_out));
            }
        }
        self.letting_go = letting_go;
        self.arrivals.retain(now, |key| seen.contains(key));
        self.invalidated.clear();
        self.mounted = mounted;
        self.apply_row_tooltips();
        self.mounted_range = window;
    }
}

/// A tree's word to the row a mounted row is: the row stands as an outline's, at the tree's own
/// height and name size, whatever decorators the app's view wrapped it in — a swipe or a hover
/// toolbar keeps the row inside it.
pub(super) fn outline_row(widget: &mut dyn crate::widget::Widget, chevron_column: f64) {
    if let Some(row) = crate::widget::find_mut::<crate::widget::ListRow>(widget) {
        row.set_outline(chevron_column);
    }
}
