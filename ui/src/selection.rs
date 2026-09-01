//! [`SelectionModel`] — a reusable index-based multi-selection model.
//!
//! Several toolkit widgets surface a list of homogeneous items that the
//! user can select — [`DataTable`](crate::widget::DataTable) list mode,
//! [`IconGrid`](crate::widget::IconGrid), the file picker, the
//! file-explorer's views — and they all need the *same* selection
//! semantics, so that logic lives here once instead of being re-derived
//! (subtly differently) in each widget.
//!
//! Pure index bookkeeping: it knows nothing about geometry, rendering, or
//! what the indices point at. Widgets map a click/keystroke to an index and
//! call [`SelectionModel::apply_click`] or [`SelectionModel::move_to`], then
//! paint `is_selected(i)` and treat `primary()` as the keyboard cursor.
//!
//! # Semantics (matching common file managers)
//!
//! | Gesture            | Effect                                             |
//! |--------------------|----------------------------------------------------|
//! | click              | select only `i`; anchor = primary = `i`            |
//! | Ctrl+click         | toggle `i`; anchor = primary = `i`                 |
//! | Shift+click        | select the inclusive range anchor..=`i` (replace)  |
//! | Ctrl+Shift+click   | add the inclusive range anchor..=`i` to the set    |
//! | arrow (no mod)     | select only the new index                          |
//! | Shift+arrow        | extend the range from the anchor to the new index  |
//!
//! When there is no anchor yet, Shift gestures degrade to a plain
//! single-select (there is nothing to extend from).

use std::collections::BTreeSet;

/// Index-based multi-selection state. Indices are opaque `usize`s the
/// owning widget assigns to its items (typically the row/cell order).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionModel {
    /// The set of currently-selected indices, kept sorted by `BTreeSet`.
    selected: BTreeSet<usize>,
    /// Range origin for Shift-extension. Set by every non-extending
    /// gesture; left untouched while a Shift gesture extends from it.
    anchor: Option<usize>,
    /// The "cursor" — the most recently touched index. Drives keyboard
    /// navigation and the focus ring. Distinct from the selected set
    /// (a Ctrl-click that *deselects* `i` still leaves `i` primary).
    primary: Option<usize>,
}

impl SelectionModel {
    /// An empty selection.
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    /// Number of selected indices.
    pub fn len(&self) -> usize {
        self.selected.len()
    }

    /// Whether `index` is currently selected.
    pub fn is_selected(&self, index: usize) -> bool {
        self.selected.contains(&index)
    }

    /// The selected indices in ascending order.
    pub fn selected(&self) -> Vec<usize> {
        self.selected.iter().copied().collect()
    }

    /// Borrow the selected set directly (ascending iteration order).
    pub fn selected_set(&self) -> &BTreeSet<usize> {
        &self.selected
    }

    /// The cursor / last-touched index, if any.
    pub fn primary(&self) -> Option<usize> {
        self.primary
    }

    /// The current range anchor, if any.
    pub fn anchor(&self) -> Option<usize> {
        self.anchor
    }

    /// Drop everything — no selection, no anchor, no cursor.
    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.primary = None;
    }

    /// Replace the selection after a model refresh while preserving the
    /// logical primary item. Callers map stable item identities to their new
    /// indices first; indices absent from `selected` cannot become primary.
    pub fn replace_indices(
        &mut self,
        selected: impl IntoIterator<Item = usize>,
        primary: Option<usize>,
    ) {
        self.selected = selected.into_iter().collect();
        self.primary = primary.filter(|index| self.selected.contains(index));
        if self.primary.is_none() {
            self.primary = self.selected.iter().next().copied();
        }
        self.anchor = self.primary;
    }

    /// Replace the selection with the single index `i`. Resets the
    /// anchor and cursor to `i`. This is the plain-click / plain-arrow
    /// behaviour and the public primitive other code can call directly.
    pub fn select_only(&mut self, i: usize) {
        self.selected.clear();
        self.selected.insert(i);
        self.anchor = Some(i);
        self.primary = Some(i);
    }

    /// Toggle `i` in/out of the selection (Ctrl-click). The anchor and
    /// cursor both move to `i` so a subsequent Shift gesture extends
    /// from here.
    pub fn toggle(&mut self, i: usize) {
        if self.selected.contains(&i) {
            self.selected.remove(&i);
        } else {
            self.selected.insert(i);
        }
        self.anchor = Some(i);
        self.primary = Some(i);
    }

    /// Select the inclusive range from the anchor to `i`: unioned with the
    /// existing selection when `additive` (Ctrl+Shift), else replacing it
    /// (Shift). The anchor is preserved so repeated Shift gestures pivot
    /// around the same origin. Falls back to [`select_only`] when there is
    /// no anchor to extend from.
    ///
    /// [`select_only`]: SelectionModel::select_only
    pub fn select_range_to(&mut self, i: usize, additive: bool) {
        let Some(anchor) = self.anchor else {
            self.select_only(i);
            return;
        };
        let (lo, hi) = if anchor <= i {
            (anchor, i)
        } else {
            (i, anchor)
        };
        if !additive {
            self.selected.clear();
        }
        for idx in lo..=hi {
            self.selected.insert(idx);
        }
        self.primary = Some(i);
    }

    /// Apply a pointer click at `index` with the given modifier state.
    /// This is the one call most widgets need from a mouse handler.
    pub fn apply_click(&mut self, index: usize, ctrl: bool, shift: bool) {
        match (ctrl, shift) {
            (_, true) => self.select_range_to(index, ctrl),
            (true, false) => self.toggle(index),
            (false, false) => self.select_only(index),
        }
    }

    /// Move the keyboard cursor to `index`. With `extend` (Shift held)
    /// it grows the contiguous range from the anchor; otherwise it
    /// becomes a single selection. This is the one call most widgets
    /// need from a key handler after computing the target index.
    pub fn move_to(&mut self, index: usize, extend: bool) {
        if extend {
            self.select_range_to(index, false);
        } else {
            self.select_only(index);
        }
    }

    /// Select every index in `0..count`. Anchor at `0`, cursor at the
    /// last index. No-op for an empty range.
    pub fn select_all(&mut self, count: usize) {
        self.selected.clear();
        if count == 0 {
            self.anchor = None;
            self.primary = None;
            return;
        }
        for i in 0..count {
            self.selected.insert(i);
        }
        self.anchor = Some(0);
        self.primary = Some(count.saturating_sub(1));
    }

    /// Drop any selected/anchor/primary index that is `>= count`. Call
    /// this after the underlying item list shrinks (e.g. a directory
    /// change) so the model never points past the end. Returns `true`
    /// if anything was pruned.
    pub fn clamp_to(&mut self, count: usize) -> bool {
        let before = self.selected.len();
        self.selected.retain(|&i| i < count);
        let pruned_set = self.selected.len() != before;
        let mut pruned_meta = false;
        if self.anchor.is_some_and(|a| a >= count) {
            self.anchor = None;
            pruned_meta = true;
        }
        if self.primary.is_some_and(|p| p >= count) {
            self.primary = None;
            pruned_meta = true;
        }
        pruned_set || pruned_meta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        let m = SelectionModel::new();
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);
        assert_eq!(m.primary(), None);
        assert_eq!(m.anchor(), None);
        assert!(m.selected().is_empty());
    }

    #[test]
    fn plain_click_replaces() {
        let mut m = SelectionModel::new();
        m.apply_click(3, false, false);
        assert_eq!(m.selected(), vec![3]);
        assert_eq!(m.anchor(), Some(3));
        assert_eq!(m.primary(), Some(3));
        // A second plain click drops the first.
        m.apply_click(7, false, false);
        assert_eq!(m.selected(), vec![7]);
        assert_eq!(m.anchor(), Some(7));
    }

    #[test]
    fn ctrl_click_toggles_and_keeps_others() {
        let mut m = SelectionModel::new();
        m.apply_click(1, false, false);
        m.apply_click(4, true, false); // add 4
        m.apply_click(6, true, false); // add 6
        assert_eq!(m.selected(), vec![1, 4, 6]);
        // Ctrl-click an already-selected index removes just it.
        m.apply_click(4, true, false);
        assert_eq!(m.selected(), vec![1, 6]);
        // The toggled index is still the cursor even though deselected.
        assert_eq!(m.primary(), Some(4));
    }

    #[test]
    fn shift_click_selects_inclusive_range_from_anchor() {
        let mut m = SelectionModel::new();
        m.apply_click(2, false, false); // anchor 2
        m.apply_click(5, false, true); // shift to 5 → 2..=5
        assert_eq!(m.selected(), vec![2, 3, 4, 5]);
        assert_eq!(m.anchor(), Some(2), "anchor stays put");
        assert_eq!(m.primary(), Some(5));
        // Shift again from the same anchor, downward — replaces range.
        m.apply_click(0, false, true);
        assert_eq!(m.selected(), vec![0, 1, 2]);
        assert_eq!(m.anchor(), Some(2));
    }

    #[test]
    fn ctrl_shift_click_adds_range() {
        let mut m = SelectionModel::new();
        m.apply_click(0, false, false);
        m.apply_click(1, false, true); // 0..=1
        // Move anchor with a ctrl-click, then ctrl+shift to union a range.
        m.apply_click(5, true, false); // anchor now 5, set {0,1,5}
        m.apply_click(7, true, true); // add 5..=7
        assert_eq!(m.selected(), vec![0, 1, 5, 6, 7]);
    }

    #[test]
    fn shift_without_anchor_is_single_select() {
        let mut m = SelectionModel::new();
        m.apply_click(4, false, true);
        assert_eq!(m.selected(), vec![4]);
        assert_eq!(m.anchor(), Some(4));
    }

    #[test]
    fn keyboard_move_replaces_then_extends() {
        let mut m = SelectionModel::new();
        m.select_only(3);
        m.move_to(4, false); // arrow down, no shift
        assert_eq!(m.selected(), vec![4]);
        assert_eq!(m.anchor(), Some(4));
        m.move_to(6, true); // shift+down twice worth
        assert_eq!(m.selected(), vec![4, 5, 6]);
        assert_eq!(m.anchor(), Some(4));
        assert_eq!(m.primary(), Some(6));
    }

    #[test]
    fn select_all_and_clear() {
        let mut m = SelectionModel::new();
        m.select_all(4);
        assert_eq!(m.selected(), vec![0, 1, 2, 3]);
        assert_eq!(m.anchor(), Some(0));
        assert_eq!(m.primary(), Some(3));
        m.clear();
        assert!(m.is_empty());
        assert_eq!(m.primary(), None);

        let mut empty = SelectionModel::new();
        empty.select_all(0);
        assert!(empty.is_empty());
        assert_eq!(empty.primary(), None);
    }

    #[test]
    fn clamp_drops_out_of_range_indices() {
        let mut m = SelectionModel::new();
        m.apply_click(1, false, false);
        m.apply_click(5, true, false);
        m.apply_click(8, true, false);
        // List shrinks to 6 items (valid indices 0..=5).
        let pruned = m.clamp_to(6);
        assert!(pruned);
        assert_eq!(m.selected(), vec![1, 5]);
        // Anchor/primary were 8 → dropped.
        assert_eq!(m.anchor(), None);
        assert_eq!(m.primary(), None);
        // A clamp that changes nothing returns false.
        assert!(!m.clamp_to(6));
    }

    #[test]
    fn model_refresh_can_replace_indices_and_keep_the_primary() {
        let mut m = SelectionModel::new();
        m.replace_indices([2, 7, 4], Some(4));
        assert_eq!(m.selected(), vec![2, 4, 7]);
        assert_eq!(m.primary(), Some(4));
        assert_eq!(m.anchor(), Some(4));

        m.replace_indices([3, 8], Some(99));
        assert_eq!(m.selected(), vec![3, 8]);
        assert_eq!(m.primary(), Some(3));
    }
}
