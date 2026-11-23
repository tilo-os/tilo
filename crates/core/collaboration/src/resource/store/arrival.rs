//! Waking the windows that read a store when another replica's change
//! arrives in it, and what the workspace said of records it asked for.

use super::{DocumentCheckout, DocumentId, ResourceStore, ResourceType};

impl ResourceStore {
    /// Wake the window `reader` belongs to whenever another replica's change
    /// arrives in this store: merged into an open record
    /// ([`Self::merge_replica`]), or kept for a record it follows
    /// ([`Self::note_kept`], [`Self::announce`]). Nothing else tells a window
    /// that its records moved, so without it what arrived waits for the
    /// window's next input or timer. A closed window's handle is dropped.
    pub fn wake_on_arrival(&self, reader: crate::WakeHandle) {
        let set = self.0.borrow();
        let mut readers = set.readers.borrow_mut();
        readers.retain(crate::WakeHandle::is_active);
        if !readers.iter().any(|known| known.same_open_scope(&reader)) {
            readers.push(reader);
        }
    }

    /// Wake every window that reads this store ([`Self::wake_on_arrival`]).
    /// Callable while a frame reads the store.
    pub(super) fn wake_readers(&self) {
        let Ok(readers) = tilo_failure::ignored!(
            "a frame holding the store is reading it and sees what arrived",
            self.0.try_borrow().map(|set| set.readers.borrow().clone())
        ) else {
            return;
        };
        for reader in readers {
            reader.wake();
        }
    }

    /// The workspace's hosts sent everything they hold of `id`, a record this
    /// store asked them for ([`DocumentId::arrives_on_demand`]), or no host is
    /// connected to ask: what of it is here is all there is for now. What
    /// reads it and still lacks it (a part held from its genesis that
    /// nothing arrived in) shows it unavailable rather than loading, until
    /// something of it arrives, which clears this. Wakes the store's
    /// readers. Callable while a frame reads the store.
    pub fn note_workspace_answered(&self, id: DocumentId) {
        self.note_workspace_answer(id, super::WorkspaceAnswer::HoldsNone);
    }

    /// The workspace's hosts answered `id`, a record this store asked them
    /// for, with `answer`: they hold none of it ([`Self::note_workspace_answered`]),
    /// or it is another person's own, which opening then says
    /// ([`super::ResourceOpenError::NotYours`]) rather than waiting for it.
    /// Wakes the store's readers. Callable while a frame reads the store.
    pub fn note_workspace_answer(&self, id: DocumentId, answer: super::WorkspaceAnswer) {
        if let Ok(set) = self.0.try_borrow() {
            set.wanted.borrow_mut().remove(&id);
            set.answered.borrow_mut().insert(id, answer);
            set.kept.set(set.kept.get().wrapping_add(1));
        }
        self.wake_readers();
    }

    /// Ask the workspace's hosts for `id`, a record a reader here knows they
    /// hold (a part before a chain's head), whether or not this store reads
    /// its records through them ([`Self::wanted_resources`]): nothing waits on
    /// it, and it opens like any record once it arrives.
    pub fn want(&self, id: DocumentId) {
        if let Ok(set) = self.0.try_borrow()
            && !set.answered.borrow().contains_key(&id)
        {
            set.wanted.borrow_mut().insert(id);
        }
    }

    /// Whether `id`, not here, is one this store asks its workspace for and the answer is
    /// still owed; it is wanted from the workspace until then
    /// ([`Self::wanted_resources`]).
    pub(super) fn awaits_workspace(&self, want: &DocumentCheckout) -> bool {
        let Ok(set) = self.0.try_borrow() else {
            return false;
        };
        if !set.asks_workspace.get() || set.answered.borrow().contains_key(&want.id) {
            return false;
        }
        set.wanted.borrow_mut().insert(want.id.clone());
        set.wanted_kinds
            .borrow_mut()
            .insert(want.id.clone(), want.kind.clone());
        true
    }

    /// The kind this store opened `id` as, when it asks its workspace for it yet
    /// ([`Self::wanted_resources`]): what a document of the app's own that its window opened
    /// from the workspace is held beside the window as when it arrives.
    #[must_use]
    pub fn wanted_kind(&self, id: &DocumentId) -> Option<ResourceType> {
        self.0
            .try_borrow()
            .ok()
            .and_then(|set| set.wanted_kinds.borrow().get(id).cloned())
    }

    /// Whether the workspace's hosts answered for `id`
    /// ([`Self::note_workspace_answer`]) and nothing of it arrived since.
    #[must_use]
    pub fn workspace_answered(&self, id: &DocumentId) -> bool {
        self.workspace_answer(id).is_some()
    }

    /// What the workspace's hosts answered for `id`, if they did and nothing
    /// of it arrived since.
    #[must_use]
    pub fn workspace_answer(&self, id: &DocumentId) -> Option<super::WorkspaceAnswer> {
        self.0
            .try_borrow()
            .ok()
            .and_then(|set| set.answered.borrow().get(id).copied())
    }

    /// A peer's change to `id` was merged: more may follow, and every window
    /// reading the store is woken, as its own merge of it finds nothing new.
    pub fn peer_change_merged(&self, id: &DocumentId) {
        if let Ok(set) = self.0.try_borrow() {
            set.answered.borrow_mut().remove(id);
        }
        self.wake_readers();
    }
}
