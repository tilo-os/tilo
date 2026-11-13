//! What a list reports as the person's choice: the rows they chose by hand, and the ones an
//! action moved onto on its way to what it opened.

use super::{Choosing, ListModel, VirtualList};

impl<M: ListModel> VirtualList<M> {
    pub(super) fn apply_selection(
        &mut self,
        key: M::Key,
        toggle: bool,
        extend: bool,
        choosing: Choosing,
    ) {
        if self.reading {
            return;
        }
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

    pub(super) fn choose_quietly(&mut self, key: M::Key) {
        if self.reading {
            return;
        }
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
        if self
            .model
            .index_of(&key)
            .is_some_and(|index| self.model.opens_in_place(index))
        {
            return;
        }
        let selected = self.selection.is_selected(&key);
        if let Some((_, handler)) = &mut self.view {
            if selected {
                handler(key);
            }
        } else if let Some(callback) = &mut self.on_select {
            callback(key, selected);
        }
    }

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
