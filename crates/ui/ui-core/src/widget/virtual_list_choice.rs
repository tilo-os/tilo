//! Choosing a row ([`VirtualRows::on_choose`]): a person's click, a key and a screen reader
//! reach the same closure.

use super::{ListKey, VirtualRows};

impl VirtualRows {
    /// Called with the row chosen, whether a click, a key or a screen reader chose it; `id`
    /// names the list, and its rows by their semantic key ([`Self::with_semantic_key`]). What is
    /// chosen is the window's own state, never an edit.
    #[must_use]
    pub fn on_choose(mut self, id: impl Into<String>, f: impl FnMut(ListKey) + 'static) -> Self {
        self.test_id = Some(id.into());
        self.view = Some(Box::new(f));
        self
    }

    #[must_use]
    pub fn open_on_press(mut self) -> Self {
        self.open_on_press = true;
        self
    }

    #[must_use]
    pub fn in_place(mut self, in_place: bool) -> Self {
        self.in_place = in_place;
        self
    }

    pub(super) fn focused_row_key(&self) -> Option<ListKey> {
        self.mounted
            .iter()
            .find(|row| crate::widget::context_menu_area::focus_within(row.widget.as_ref()))
            .map(|row| row.key.clone())
            .or_else(|| self.focused_key.clone())
    }
}
