//! Ordered timeline selection with a stable primary editing anchor.

use crate::model::{ClipId, Project};

/// The timeline's clip selection: an ordered set with a *primary* (anchor)
/// clip. The primary is what the inspector edits and what a fresh click
/// re-anchors to; every clip in the set takes part in delete / ripple-delete /
/// copy / cut / duplicate / nudge / split, each of which lands as **one** undo
/// boundary regardless of how many clips it touched.
#[derive(Clone, Default, PartialEq)]
pub(super) struct Selection {
    ids: Vec<ClipId>,
    primary: Option<ClipId>,
}

impl Selection {
    pub(super) fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
    pub(super) fn len(&self) -> usize {
        self.ids.len()
    }
    /// The anchor clip — the one the inspector shows and single-clip edits act
    /// on. `None` exactly when the selection is empty.
    pub(super) fn primary(&self) -> Option<ClipId> {
        self.primary
    }
    pub(super) fn contains(&self, id: ClipId) -> bool {
        self.ids.contains(&id)
    }
    pub(super) fn ids(&self) -> &[ClipId] {
        &self.ids
    }
    pub(super) fn clear(&mut self) {
        self.ids.clear();
        self.primary = None;
    }
    /// Collapse to just `id` — a plain click.
    pub(super) fn select_only(&mut self, id: ClipId) {
        self.ids.clear();
        self.ids.push(id);
        self.primary = Some(id);
    }
    /// Add or remove `id` (shift-click). Removing the primary promotes
    /// whatever is left so the inspector never points at nothing while clips
    /// are still selected.
    pub(super) fn toggle(&mut self, id: ClipId) {
        if let Some(pos) = self.ids.iter().position(|&c| c == id) {
            self.ids.remove(pos);
            if self.primary == Some(id) {
                self.primary = self.ids.last().copied();
            }
        } else {
            self.ids.push(id);
            self.primary = Some(id);
        }
    }
    /// Re-anchor on an already-selected clip without disturbing the set — a
    /// plain click inside a multi-selection.
    pub(super) fn set_primary(&mut self, id: ClipId) {
        if self.ids.contains(&id) {
            self.primary = Some(id);
        }
    }
    /// Drop every id in `gone` (clips a track / media deletion is about to
    /// take with it), promoting the primary if it was one of them.
    pub(super) fn remove_all(&mut self, gone: &[ClipId]) {
        self.ids.retain(|id| !gone.contains(id));
        if self.primary.is_some_and(|id| gone.contains(&id)) {
            self.primary = self.ids.last().copied();
        }
    }
    /// Replace the whole set (a marquee release), anchoring on the last id.
    pub(super) fn set(&mut self, ids: Vec<ClipId>) {
        self.primary = ids.last().copied();
        self.ids = ids;
    }
    /// Replace the whole set while choosing its editing anchor explicitly.
    /// A stale requested anchor falls back to the final member so the primary
    /// is never outside a non-empty selection.
    pub(super) fn set_with_primary(&mut self, ids: Vec<ClipId>, primary: Option<ClipId>) {
        self.primary = primary
            .filter(|candidate| ids.contains(candidate))
            .or_else(|| ids.last().copied());
        self.ids = ids;
    }
    /// Drop ids whose clips are gone (a remote delete, or a track deletion),
    /// promoting the primary if it was one of them.
    pub(super) fn retain_existing(&mut self, p: &Project) {
        self.ids.retain(|id| p.clip(*id).is_some());
        if self.primary.is_some_and(|id| p.clip(id).is_none()) {
            self.primary = self.ids.last().copied();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn selection_primary_stays_a_member_of_the_selection() {
        // Case `removing_the_primary_promotes_the_last_remaining_clip`.
        {
            let first = Uuid::new_v4();
            let second = Uuid::new_v4();
            let mut selection = Selection::default();
            selection.set(vec![first, second]);

            selection.toggle(second);

            assert_eq!(selection.ids(), &[first]);
            assert_eq!(selection.primary(), Some(first));
        }
        // Case `an_explicit_primary_must_belong_to_the_selection`.
        {
            let first = Uuid::new_v4();
            let second = Uuid::new_v4();
            let stale = Uuid::new_v4();
            let mut selection = Selection::default();

            selection.set_with_primary(vec![first, second], Some(stale));

            assert_eq!(selection.ids(), &[first, second]);
            assert_eq!(selection.primary(), Some(second));
        }
    }
}
