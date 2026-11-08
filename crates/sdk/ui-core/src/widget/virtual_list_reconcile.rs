//! How a [`super::VirtualList`] takes a change to its model and mounts the
//! rows that change leaves: the rows that stay keep their widgets where they
//! stand, the rows built anew are mounted in their place, and the rows the
//! list lets go of keep the layout walk's place until a remount unmounts them.

use std::collections::{HashMap, HashSet};

use super::{
    ListModel, ListModelChange, MountedRow, MountedRowState, RowBuild, RowSelection, VirtualList,
    groups,
};
use crate::virtual_viewport::SparseAnchor;

impl<M: ListModel> VirtualList<M> {
    pub fn update_model<R>(&mut self, update: impl FnOnce(&mut M) -> R) -> R {
        let anchor = self.capture_anchor();
        let result = update(&mut self.model);
        self.forget_tree();
        let dropped = std::mem::take(&mut self.mounted);
        self.let_go_of_rows(dropped);
        self.structure_dirty = true;
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
        self.forget_tree();
        self.apply_model_changes(changes);
        self.reconcile_model(anchor.as_ref());
        result
    }

    pub(super) fn reconcile_model(&mut self, anchor: Option<&SparseAnchor<M::Key>>) {
        // What the rows were grouped by may have moved with them.
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
        // A heading the rows moved away from lets the keyboard go.
        self.focused_group = self.focused_group.take().filter(|(heading, index)| {
            groups::group_of(&self.model, *index, self.tail_title.as_deref())
                .is_some_and(|group| group.heading == *heading)
        });
        self.request_dirty = true;
        self.keep_end();
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
        let source = crate::collection_transition::RowMotions::<M::Key>::caused_by_hand(
            crate::time::now(),
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

    /// Start the motion of a change noted before this frame, and forget
    /// rows that have arrived. A row already moving continues from where it
    /// is drawn.
    pub(super) fn start_row_motion(&mut self, motion: crate::tokens::EffectiveMotion) {
        let shown = self
            .mounted
            .iter()
            .filter_map(|row| Some((row.key.clone(), self.viewport.item_view_top(row.index)?)));
        self.row_motion.start(shown, motion, crate::time::now());
    }

    fn apply_model_changes(&mut self, changes: &[ListModelChange<M::Key>]) {
        if changes
            .iter()
            .any(|change| matches!(change, ListModelChange::Reset))
        {
            let dropped = std::mem::take(&mut self.mounted);
            self.let_go_of_rows(dropped);
            // The rows come back built anew, where they stand: the layout
            // walk still holds the ones let go of until a remount mounts
            // the new ones in their places.
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

    /// The rows `dropped` leave the list: their widgets stay alive where
    /// the retained layout holds their nodes, so the layout walk finds
    /// them until a remount unmounts them ([`Self::remount_rows`]).
    fn let_go_of_rows(&mut self, dropped: Vec<MountedRow<M::Key>>) {
        for row in dropped {
            let place = row.index;
            if let Some(laid_out) = row.laid_out() {
                self.letting_go.push((place, laid_out));
            }
        }
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
        // Rows whose content changed, and nothing else: every key stays where it was, so the
        // changed rows are built anew and mounted in place ([`Self::invalidate_rows`]), not the
        // list rebuilt.
        if changes
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

    /// Close the row standing open when its item left the list: what the person is reading
    /// where it stands is always a row of the list they see, so an item that goes — to another
    /// place, another query, away — closes with it.
    pub(super) fn close_open_leaving(&mut self) {
        let Some(open) = self.open.as_ref() else {
            return;
        };
        if self.model.index_of(open).is_some() {
            return;
        }
        let open = self.open.take();
        // The row that went takes its growth with it: nothing measures it again, and a
        // height on its way nowhere would ask for frames that never settle.
        if let Some(open) = &open {
            self.growing.remove(open);
            self.growth.retain(|(grown, _)| grown != open);
        }
        // A host keeping which item is open hears it close with its row, so the list it builds
        // for the place the person moved to holds nothing open.
        self.report_open();
    }

    pub fn invalidate_rows(&mut self, keys: impl IntoIterator<Item = M::Key>) {
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
            // Built anew and mounted where they stand, as rows a scroll
            // brings near are: a changed row costs its own layout and paint,
            // not the window's (`StructureChange::Children`). A row that
            // comes back a different height moves the others, which the
            // remount measures and lays out.
            self.remount = true;
            self.layout_dirty = true;
        }
    }

    pub(super) fn capture_anchor(&self) -> Option<SparseAnchor<M::Key>> {
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
        // What the tree rows are, before any of them is built: a row reads its depth and
        // whether its subtasks show from what this leaves in its marks.
        self.shape_tree(&range);
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
                // A closed group: its rows take no room and are never built, and its
                // heading's band stands where its first row would (placed with the bands).
                if self.row_starts(index) {
                    // The heading still takes its band's room: it is what the person presses
                    // to open the group again.
                    self.groups.fold(index);
                    let band = self.groups.height;
                    // ratchet-allow(legacy_size_api): the row's height, not a text's, is measured.
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
                // Shown again, a row measures as it did: the room it takes is its own again.
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
            // A row the model re-keyed stands where its row stood: the
            // layout walk holds the row let go of at that place.
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
                        // A row that paints the selection it is handed takes the new one.
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
                    let widget = match &mounted_state {
                        MountedRowState::Ready => (self.row_builder)(
                            &self.model,
                            index,
                            &key,
                            RowBuild {
                                selection,
                                expanded: self.open.as_ref() == Some(&key),
                            },
                        ),
                        state => self.arriving_row(index, state),
                    };
                    self.row_builds = self.row_builds.wrapping_add(1);
                    MountedRow {
                        widget,
                        key: key.clone(),
                        index,
                        stamp: self.row_builds,
                        state: mounted_state,
                        selection,
                        node: None,
                        // The retained layout holds the row it replaces
                        // until a remount lets go of it.
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
            // The same rows, some built anew (their content or selection
            // changed): each new one is mounted where the old one stood
            // (`remount_rows`), and the others keep their layout.
            self.remount = true;
            self.layout_dirty = true;
        }
        // The rows left over are where the layout walk still holds their
        // nodes, until the remount takes them away.
        let mut letting_go = std::mem::take(&mut self.letting_go);
        for (_, row) in old {
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
