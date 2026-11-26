//! Following the parts a window holds: reading again the ones that changed,
//! and taking up a part that arrived. A chain skips this while its store takes
//! no write, so what a pass follows in that case is the parts' own versions.

use super::ChainCore;
use ui_core::resource::{DocumentId, WorkspaceAnswer};

impl ChainCore {
    /// Follow every held part that changed, and take up a newer head that
    /// arrived on this device. Whether anything moved.
    pub(super) fn follow_parts(&self) -> bool {
        let mut moved = false;
        let generation = self.workspace.store.write_generation();
        // Nothing held changes while the store takes no write.
        if generation == u64::MAX || self.followed_at.get() != Some(generation) {
            moved = self.follow_held();
            if self.part.held_whole {
                self.hold_earlier();
                self.earlier_at
                    .set(Some(self.workspace.store.kept_revision()));
            }
            self.followed_at
                .set(Some(self.workspace.store.write_generation()));
        } else {
            // Following them is skipped, and skipped they would be read by no
            // pass at all: read each held part's version, without syncing it,
            // so the window follows its own parts and syncs again when one of
            // them changes. A write elsewhere in the store, another window's
            // document, reads the same.
            self.read_held();
            // An earlier part arrives without a write to this store, so the
            // write generation says nothing of it: what its arrival moves is
            // the store's kept revision.
            if self.part.held_whole
                && self.earlier_at.get() != Some(self.workspace.store.kept_revision())
            {
                self.hold_earlier();
                self.earlier_at
                    .set(Some(self.workspace.store.kept_revision()));
            }
        }
        if self.due() {
            moved |= self.look();
            moved |= self.follow_waiting();
        }
        moved |= self.open_in_turn();
        moved
    }

    /// Read each held part's document version, without syncing what it holds:
    /// what a window's pass follows while [`Self::follow_held`] is skipped, so
    /// a later pass takes up a part it holds that changed.
    fn read_held(&self) {
        // Outside a pass there is nothing to follow, and once in a pass is
        // enough: later looks the pass takes read the same parts.
        let Some(pass) = tilo_ui_base::component_inputs::running_pass() else {
            return;
        };
        if self.read_at.get() == Some(pass) {
            return;
        }
        let parts = self.parts.borrow();
        let mut read = true;
        for index in parts.keys() {
            if self
                .workspace
                .store
                .read(&self.chain.part(*index), |_| ())
                .is_none()
            {
                read = false;
            }
        }
        if read {
            self.read_at.set(Some(pass));
        }
    }

    /// Take `part` as new when the workspace answered that it holds no part of
    /// the chain and none is held here: its answer for the chain is its answer
    /// for every part, so the first part is started at once. Asked for
    /// instead, a write into a new chain waited for a second answer the
    /// workspace had already given.
    pub(super) fn start_where_none_is_held(&self, part: &DocumentId) {
        let store = &self.workspace.store;
        if self.parts.borrow().is_empty()
            && !store.is_ready(part)
            && store.workspace_answer(&self.chain.chain_head()) == Some(WorkspaceAnswer::HoldsNone)
        {
            store.note_workspace_answer(part.clone(), WorkspaceAnswer::HoldsNone);
        }
    }
}
