//! A row's rename, delete and buttons as published actions
//! ([`TreeImpl::on_rename_action`], [`TreeImpl::on_delete_action`],
//! [`TreeImpl::on_row_action_action`]): a key, assistive technology, a script
//! and an agent each ask the host for the action the app bound, with the row
//! it was on, so the app's own action confirms, undoes and permits the change
//! like every other.

use super::{NodeId, TreeImpl, TreeModel};
use crate::widget::ActionEmitter;

impl<T, M: TreeModel> TreeImpl<T, M> {
    /// Renaming a row on the app's own action: `Enter` in the inline field,
    /// [`Self::commit_rename`] and a double click report the row's semantic
    /// key and the trimmed title to the host, which runs the action.
    #[must_use]
    pub fn on_rename_action(
        mut self,
        binding: tilo_actions::ActionBinding<(String, String)>,
    ) -> Self {
        self.bind_rename(binding);
        self
    }

    /// [`Self::on_rename_action`], on a tree already built.
    pub fn bind_rename(&mut self, binding: tilo_actions::ActionBinding<(String, String)>) {
        self.rename_action = Some(ActionEmitter::new(binding));
    }

    /// Delete on the focused row on the app's own action, by semantic key.
    #[must_use]
    pub fn on_delete_action(mut self, binding: tilo_actions::ActionBinding<String>) -> Self {
        self.bind_delete(binding);
        self
    }

    /// [`Self::on_delete_action`], on a tree already built.
    pub fn bind_delete(&mut self, binding: tilo_actions::ActionBinding<String>) {
        self.delete_action = Some(ActionEmitter::new(binding));
    }

    /// A row's button on the app's own action, one binding for the button at
    /// each index of [`Self::with_row_actions`], by the row's semantic key. A
    /// button that opens the row's menu reports nothing, as it does to
    /// [`Self::on_row_action`].
    #[must_use]
    pub fn on_row_actions_action(
        mut self,
        bindings: impl IntoIterator<Item = (usize, tilo_actions::ActionBinding<String>)>,
    ) -> Self {
        self.bind_row_actions(bindings);
        self
    }

    /// [`Self::on_row_actions_action`], on a tree already built.
    pub fn bind_row_actions(
        &mut self,
        bindings: impl IntoIterator<Item = (usize, tilo_actions::ActionBinding<String>)>,
    ) {
        for (index, binding) in bindings {
            if self.row_action_actions.len() <= index {
                self.row_action_actions.resize_with(index + 1, || None);
            }
            self.row_action_actions[index] = Some(ActionEmitter::new(binding));
        }
    }

    /// Whether the tree renames in place: [`Self::on_rename_action`] or
    /// [`Self::on_rename`].
    pub(super) fn renames(&self) -> bool {
        self.rename_action.is_some() || self.on_rename.is_some()
    }

    /// Whether the tree reports `Delete` on a row: [`Self::on_delete_action`]
    /// or [`Self::on_delete`].
    pub(super) fn deletes(&self) -> bool {
        self.delete_action.is_some() || self.on_delete.is_some()
    }

    /// Report that row `id` was renamed to `title`.
    pub(super) fn report_rename(&mut self, id: NodeId, title: String) {
        let key = self.entry_key(id);
        if let Some(emitter) = self.rename_action.as_mut() {
            if let Err(error) = emitter.emit((key, title)) {
                tracing::warn!(%error, "a rename refused its row");
            }
        } else if let Some(callback) = self.on_rename.as_mut() {
            callback(id, title);
        }
    }

    /// Report that `Delete` was pressed on row `id`.
    pub(super) fn report_delete(&mut self, id: NodeId) {
        let key = self.entry_key(id);
        if let Some(emitter) = self.delete_action.as_mut() {
            if let Err(error) = emitter.emit(key) {
                tracing::warn!(%error, "a delete refused its row");
            }
        } else if let Some(callback) = self.on_delete.as_mut() {
            callback(id);
        }
    }

    /// Report that button `index` of row `id` was pressed.
    pub(super) fn report_row_action(&mut self, id: NodeId, index: usize) {
        let key = self.entry_key(id);
        if let Some(emitter) = self
            .row_action_actions
            .get_mut(index)
            .and_then(Option::as_mut)
        {
            if let Err(error) = emitter.emit(key) {
                tracing::warn!(%error, "a row action refused its row");
            }
        } else if let Some(callback) = self.on_row_action.as_mut() {
            callback(id, index);
        }
    }
}
