//! Shared collection selection models: [`SelectionModel`], the compact index-based form
//! for immutable local lists, and [`KeySelection`], which keeps selection attached to
//! stable model keys and represents select-all as a complement, for mutable, virtualized
//! and partially resident collections. Several toolkit widgets surface a list of
//! homogeneous items to select — `DataTable` list mode, the file picker, the file
//! explorer's views — and they all need the *same* selection semantics, so that logic
//! lives here once instead of being re-derived in each widget.

use std::collections::{BTreeSet, HashSet};
use std::hash::Hash;

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeySelectionMode<K: Eq + Hash> {
    Explicit(HashSet<K>),
    AllExcept(HashSet<K>),
}

/// Stable-key multi-selection for virtualized and mutable collections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeySelection<K: Eq + Hash> {
    mode: KeySelectionMode<K>,
    anchor: Option<K>,
    primary: Option<K>,
}

impl<K: Eq + Hash> Default for KeySelection<K> {
    fn default() -> Self {
        Self {
            mode: KeySelectionMode::Explicit(HashSet::new()),
            anchor: None,
            primary: None,
        }
    }
}

impl<K> KeySelection<K>
where
    K: Clone + Eq + Hash,
{
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_selected(&self, key: &K) -> bool {
        match &self.mode {
            KeySelectionMode::Explicit(selected) => selected.contains(key),
            KeySelectionMode::AllExcept(excluded) => !excluded.contains(key),
        }
    }

    #[must_use]
    pub fn is_select_all(&self) -> bool {
        matches!(self.mode, KeySelectionMode::AllExcept(_))
    }

    /// Number selected given the current number of selectable model keys.
    #[must_use]
    pub fn len(&self, total: usize) -> usize {
        match &self.mode {
            KeySelectionMode::Explicit(selected) => selected.len(),
            KeySelectionMode::AllExcept(excluded) => total.saturating_sub(excluded.len()),
        }
    }

    #[must_use]
    pub fn is_empty(&self, total: usize) -> bool {
        self.len(total) == 0
    }

    #[must_use]
    pub fn anchor(&self) -> Option<&K> {
        self.anchor.as_ref()
    }

    #[must_use]
    pub fn primary(&self) -> Option<&K> {
        self.primary.as_ref()
    }

    /// The finite stored set: selected keys in explicit mode, deselected keys in
    /// select-all mode.
    pub fn stored_keys(&self) -> impl Iterator<Item = &K> {
        match &self.mode {
            KeySelectionMode::Explicit(keys) | KeySelectionMode::AllExcept(keys) => keys.iter(),
        }
    }

    pub fn clear(&mut self) {
        self.mode = KeySelectionMode::Explicit(HashSet::new());
        self.anchor = None;
        self.primary = None;
    }

    pub fn select_only(&mut self, key: K) {
        self.mode = KeySelectionMode::Explicit(HashSet::from([key.clone()]));
        self.anchor = Some(key.clone());
        self.primary = Some(key);
    }

    pub fn toggle(&mut self, key: K) {
        match &mut self.mode {
            KeySelectionMode::Explicit(selected) => {
                if !selected.remove(&key) {
                    selected.insert(key.clone());
                }
            }
            KeySelectionMode::AllExcept(excluded) => {
                if !excluded.remove(&key) {
                    excluded.insert(key.clone());
                }
            }
        }
        if self.anchor.is_none() {
            self.anchor = Some(key.clone());
        }
        self.primary = Some(key);
    }

    /// Apply an inclusive key range already resolved by the owning model.
    pub fn select_range<I>(&mut self, target: K, keys: I, additive: bool)
    where
        I: IntoIterator<Item = K>,
    {
        if self.anchor.is_none() {
            self.select_only(target);
            return;
        }
        if additive {
            match &mut self.mode {
                KeySelectionMode::Explicit(selected) => selected.extend(keys),
                KeySelectionMode::AllExcept(excluded) => {
                    for key in keys {
                        excluded.remove(&key);
                    }
                }
            }
        } else {
            self.mode = KeySelectionMode::Explicit(keys.into_iter().collect());
        }
        self.primary = Some(target);
    }

    pub fn select_all(&mut self) {
        self.mode = KeySelectionMode::AllExcept(HashSet::new());
        self.anchor = None;
        self.primary = None;
    }

    /// Drop keys no longer present after a model change.
    pub fn reconcile<F>(&mut self, mut contains: F)
    where
        F: FnMut(&K) -> bool,
    {
        match &mut self.mode {
            KeySelectionMode::Explicit(selected) | KeySelectionMode::AllExcept(selected) => {
                selected.retain(|key| contains(key));
            }
        }
        if self.anchor.as_ref().is_some_and(|key| !contains(key)) {
            self.anchor = None;
        }
        if self.primary.as_ref().is_some_and(|key| !contains(key)) {
            self.primary = None;
        }
    }
}

/// Index-based multi-selection state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionModel {
    selected: BTreeSet<usize>,
    anchor: Option<usize>,
    primary: Option<usize>,
}

impl SelectionModel {
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

    /// Follow items when their owning collection changes row order.
    pub fn remap_indices(&mut self, map: impl Fn(usize) -> Option<usize>) {
        self.selected = self.selected.iter().copied().filter_map(&map).collect();
        self.primary = self.primary.and_then(&map);
        self.anchor = self.anchor.and_then(map);
    }

    /// Drop everything — no selection, no anchor, no cursor.
    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
        self.primary = None;
    }

    /// Replace the selection after a model refresh while preserving the logical primary
    /// item. Callers map stable item identities to their new indices first; indices
    /// absent from `selected` cannot become primary.
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

    /// Replace the selection with the single index `i`.
    pub fn select_only(&mut self, i: usize) {
        self.selected.clear();
        self.selected.insert(i);
        self.anchor = Some(i);
        self.primary = Some(i);
    }

    /// Toggle `i` in/out of the selection (Ctrl-click).
    pub fn toggle(&mut self, i: usize) {
        if self.selected.contains(&i) {
            self.selected.remove(&i);
        } else {
            self.selected.insert(i);
        }
        if self.anchor.is_none() {
            self.anchor = Some(i);
        }
        self.primary = Some(i);
    }

    /// Select the inclusive range from the anchor to `i`: unioned with the existing
    /// selection when `additive` (Ctrl+Shift), else replacing it (Shift).
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
    pub fn apply_click(&mut self, index: usize, ctrl: bool, shift: bool) {
        match (ctrl, shift) {
            (_, true) => self.select_range_to(index, ctrl),
            (true, false) => self.toggle(index),
            (false, false) => self.select_only(index),
        }
    }

    /// Move the keyboard cursor to `index`.
    pub fn move_to(&mut self, index: usize, extend: bool) {
        if extend {
            self.select_range_to(index, false);
        } else {
            self.select_only(index);
        }
    }

    /// Select every index in `0..count`.
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

    /// Drop any selected/anchor/primary index that is `>= count`. Call this after the
    /// underlying item list shrinks (e.g. a directory change) so the model never points
    /// past the end.
    pub fn clamp_to(&mut self, count: usize) -> bool {
        let before = self.selected.len();
        self.selected.retain(|&i| i < count);
        let pruned_set = self.selected.len() != before;
        let pruned_anchor = self.anchor.take_if(|a| *a >= count).is_some();
        let pruned_primary = self.primary.take_if(|p| *p >= count).is_some();
        pruned_set || pruned_anchor || pruned_primary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyed_selection_tracks_keys_through_reorder_select_all_and_ranges() {
        {
            let mut selection = KeySelection::new();
            selection.select_only("beta");
            selection.select_range("delta", ["beta", "gamma", "delta"], false);

            let reordered = ["delta", "alpha", "gamma", "beta"];
            assert!(selection.is_selected(&reordered[0]));
            assert!(selection.is_selected(&reordered[2]));
            assert_eq!(selection.primary(), Some(&"delta"));

            selection.reconcile(|key| *key != "gamma");
            assert!(!selection.is_selected(&"gamma"));
            assert_eq!(selection.len(3), 2);
        }

        {
            let mut selection = KeySelection::new();
            selection.select_all();
            assert!(selection.is_select_all());
            assert_eq!(selection.stored_keys().count(), 0);
            assert_eq!(selection.len(100_000_000), 100_000_000);

            selection.toggle(42_u64);
            assert!(!selection.is_selected(&42));
            assert!(selection.is_selected(&43));
            assert_eq!(
                selection.stored_keys().copied().collect::<Vec<_>>(),
                vec![42]
            );
            assert_eq!(selection.len(100_000_000), 99_999_999);

            selection.toggle(42);
            assert!(selection.is_selected(&42));
            assert_eq!(selection.stored_keys().count(), 0);
        }

        {
            let mut selection = KeySelection::new();
            selection.select_all();
            selection.toggle(2_u64);
            selection.toggle(3_u64);
            selection.select_range(3, [1, 2, 3], true);

            assert!(selection.is_select_all());
            assert!(selection.is_selected(&2));
            assert!(selection.is_selected(&3));
            assert_eq!(selection.primary(), Some(&3));
        }
    }

    #[test]
    fn remapping_selection_preserves_an_unselected_cursor_and_drops_missing_items() {
        let mut selection = SelectionModel::new();
        selection.apply_click(0, false, false);
        selection.apply_click(2, true, false);
        selection.apply_click(2, true, false);
        selection.remap_indices(|index| [Some(3), None, Some(1)].get(index).copied().flatten());
        assert_eq!(selection.selected(), vec![3]);
        assert_eq!(selection.primary(), Some(1));
        assert_eq!(
            selection.anchor(),
            Some(3),
            "the anchor is where the choice began, not where the modified click moved the cursor"
        );
        selection.apply_click(2, false, true);
        assert_eq!(selection.selected(), vec![2, 3]);
        selection.remap_indices(|index| (index == 2).then_some(0));
        assert_eq!(selection.selected(), vec![0]);
        assert_eq!(selection.primary(), Some(0));
        assert_eq!(selection.anchor(), None);
    }

    /// What must hold after one click of a scripted sequence.
    enum Expect {
        Selected(&'static [usize]),
        Anchor(Option<usize>),
        Primary(Option<usize>),
    }

    struct Click {
        index: usize,
        ctrl: bool,
        shift: bool,
        expect: Vec<Expect>,
    }

    fn click(index: usize, ctrl: bool, shift: bool) -> Click {
        Click {
            index,
            ctrl,
            shift,
            expect: Vec::new(),
        }
    }

    impl Click {
        fn selects(mut self, selected: &'static [usize]) -> Self {
            self.expect.push(Expect::Selected(selected));
            self
        }
        fn anchor(mut self, anchor: Option<usize>) -> Self {
            self.expect.push(Expect::Anchor(anchor));
            self
        }
        fn primary(mut self, primary: Option<usize>) -> Self {
            self.expect.push(Expect::Primary(primary));
            self
        }
    }

    #[test]
    fn click_sequences_replace_toggle_and_extend_from_the_anchor() {
        let m = SelectionModel::new();
        assert!(m.is_empty());
        assert_eq!(m.len(), 0);
        assert_eq!(m.primary(), None);
        assert_eq!(m.anchor(), None);
        assert!(m.selected().is_empty());

        let cases: [(&str, Vec<Click>); 5] = [
            (
                // A second plain click drops the first.
                "plain click replaces",
                vec![
                    click(3, false, false)
                        .selects(&[3])
                        .anchor(Some(3))
                        .primary(Some(3)),
                    click(7, false, false).selects(&[7]).anchor(Some(7)),
                ],
            ),
            (
                "ctrl click toggles and keeps others",
                vec![
                    click(1, false, false),
                    click(4, true, false),
                    click(6, true, false).selects(&[1, 4, 6]),
                    click(4, true, false).selects(&[1, 6]).primary(Some(4)),
                ],
            ),
            (
                "shift click selects an inclusive range from the anchor",
                vec![
                    click(2, false, false),
                    click(5, false, true)
                        .selects(&[2, 3, 4, 5])
                        .anchor(Some(2))
                        .primary(Some(5)),
                    click(0, false, true).selects(&[0, 1, 2]).anchor(Some(2)),
                ],
            ),
            (
                "ctrl shift click adds a range",
                vec![
                    click(0, false, false),
                    click(1, false, true),
                    click(5, true, false).anchor(Some(0)),
                    click(7, true, true)
                        .selects(&[0, 1, 2, 3, 4, 5, 6, 7])
                        .anchor(Some(0)),
                ],
            ),
            (
                "shift without an anchor is a single select",
                vec![click(4, false, true).selects(&[4]).anchor(Some(4))],
            ),
        ];
        for (case, clicks) in cases {
            let mut m = SelectionModel::new();
            for (step, c) in clicks.iter().enumerate() {
                m.apply_click(c.index, c.ctrl, c.shift);
                for expect in &c.expect {
                    match *expect {
                        Expect::Selected(selected) => {
                            assert_eq!(
                                m.selected(),
                                selected,
                                "case {case}, step {step}: selected"
                            );
                        }
                        Expect::Anchor(anchor) => {
                            assert_eq!(m.anchor(), anchor, "case {case}, step {step}: anchor");
                        }
                        Expect::Primary(primary) => {
                            assert_eq!(m.primary(), primary, "case {case}, step {step}: primary");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn keyboard_select_all_clamp_and_refresh_keep_anchor_and_primary_consistent() {
        {
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

        {
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

        {
            let mut m = SelectionModel::new();
            m.apply_click(1, false, false);
            m.apply_click(5, true, false);
            m.apply_click(8, true, false);
            // List shrinks to 6 items (valid indices 0..=5).
            let pruned = m.clamp_to(6);
            assert!(pruned);
            assert_eq!(m.selected(), vec![1, 5]);
            // The cursor at 8 is gone with the item; the anchor at 1 still stands.
            assert_eq!(m.anchor(), Some(1));
            assert_eq!(m.primary(), None);
            assert!(!m.clamp_to(6));
        }

        {
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

    #[test]
    fn the_walk_extends_the_choice_modified_clicks_gathered() {
        {
            let mut m = SelectionModel::new();
            m.apply_click(0, false, false);
            m.apply_click(1, true, false);
            assert_eq!(m.selected(), vec![0, 1]);
            m.move_to(2, true);
            assert_eq!(m.selected(), vec![0, 1, 2]);
            assert_eq!(m.anchor(), Some(0));
            assert_eq!(m.primary(), Some(2));
            m.move_to(1, true);
            assert_eq!(m.selected(), vec![0, 1]);
        }

        {
            let mut selection = KeySelection::new();
            selection.select_only("a");
            selection.toggle("b");
            selection.select_range("c", ["a", "b", "c"], false);
            let mut held: Vec<&str> = selection.stored_keys().copied().collect();
            held.sort_unstable();
            assert_eq!(held, ["a", "b", "c"]);
            assert_eq!(selection.anchor(), Some(&"a"));
            assert_eq!(selection.primary(), Some(&"c"));
        }

        {
            let mut m = SelectionModel::new();
            m.apply_click(4, true, false);
            assert_eq!(m.anchor(), Some(4));
            m.move_to(5, true);
            assert_eq!(m.selected(), vec![4, 5]);
        }
    }
}
