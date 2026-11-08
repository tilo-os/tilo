//! What a list reports as the person's choice: the rows they chose by hand, and the ones an
//! action moved onto on its way to what it opened.

use super::{Choosing, ListModel, VirtualList};

impl<M: ListModel> VirtualList<M> {
    /// Choose `key` as the gesture says: `toggle` adds it to a choice or takes it away, and
    /// `extend` grows the choice to it. A gesture that only changes a choice does not open
    /// what it touches, so only a plain choice tells the row's listener it was chosen.
    pub(super) fn apply_selection(
        &mut self,
        key: M::Key,
        toggle: bool,
        extend: bool,
        choosing: Choosing,
    ) {
        let model = &self.model;
        crate::widget::rows::choose(
            &mut self.selection,
            key.clone(),
            (toggle, extend),
            self.multi_select,
            |key| model.index_of(key),
            |index| model.key_at(index),
        );
        if !toggle && !extend {
            self.emit_selection(key);
        }
        self.report_selection(choosing);
    }

    /// Move the selection onto `key` without reporting the choice: an action that opens
    /// the row moves it there on the way, and the opening is what the host hears.
    pub(super) fn choose_quietly(&mut self, key: M::Key) {
        let model = &self.model;
        crate::widget::rows::choose(
            &mut self.selection,
            key,
            (false, false),
            self.multi_select,
            |key| model.index_of(key),
            |index| model.key_at(index),
        );
        self.report_selection(Choosing::Following);
    }

    pub(super) fn emit_selection(&mut self, key: M::Key) {
        // A row that opens where it stands shows what it was chosen for under itself: the
        // item is the document the list just opened, not something the host paints elsewhere.
        if self
            .model
            .index_of(&key)
            .is_some_and(|index| self.model.opens_in_place(index))
        {
            return;
        }
        let selected = self.selection.is_selected(&key);
        if let Some((_, handler)) = &mut self.view {
            // The person's choice is the published view action's own run:
            // no host decides a view or records it.
            if selected {
                handler(key);
            }
        } else if let Some(callback) = &mut self.on_select {
            callback(key, selected);
        }
    }

    /// Tell a listener which rows are selected now, in the order they stand.
    ///
    /// What a window keeps of a choice is a choice the person made by hand, a choice of more
    /// than one row, and the way back from one of many to one row or none: a selection that
    /// only followed the focus or the row that opened is the list's own.
    pub(super) fn report_selection(&mut self, choosing: Choosing) {
        if self.on_selected.is_none() {
            return;
        }
        let keys = self.selected_keys();
        let many = keys.len() > 1;
        let worth = choosing == Choosing::ByHand || many || self.reported_many;
        self.reported_many = many;
        if !worth {
            return;
        }
        if let Some(callback) = &mut self.on_selected {
            callback(&keys);
        }
    }

    /// Every selected row's key, in the order the rows stand: a select-all names all of them.
    pub(super) fn selected_keys(&self) -> Vec<M::Key> {
        let mut keys: Vec<M::Key> = if self.selection.is_select_all() {
            (0..self.model.len())
                .filter_map(|index| self.model.key_at(index))
                .filter(|key| self.selection.is_selected(key))
                .collect()
        } else {
            self.selection.stored_keys().cloned().collect()
        };
        keys.sort_by_key(|key| self.model.index_of(key));
        keys
    }
}
