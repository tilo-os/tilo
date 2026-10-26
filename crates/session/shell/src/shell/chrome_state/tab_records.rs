//! Which record a freshly opened tab gets, and the bounded retry of that
//! creation when the store is opening it on its own thread.

use super::{ChromeState, PendingTabRecordCreation, TabRecordCreationIdentity};

impl ChromeState {
    /// Finalize a new-tab action after its complete document reference/setup.
    /// Only transient record-loan contention retains a bounded retry ticket.
    pub(crate) fn create_new_tab_record(&mut self, tab: u64) {
        let tree = self.tree();
        let Some(identity) = TabRecordCreationIdentity::capture(&tree, tab) else {
            return;
        };
        let tile = tree
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|current| current.id == tab)
            .map(|current| current.tile.clone());
        drop(tree);
        let Some(tile) = tile else {
            return;
        };
        // A mounted app's definition makes its record when the component
        // mounts; only content the compositor shows itself keeps a scratch
        // tab record.
        let mounts_apps = self.hosted_inputs.in_process().is_some();
        let Some(checkout) = crate::shell::new_tab_record(&tile, tab, mounts_apps) else {
            return;
        };
        match self.records.create_record(&checkout) {
            Ok(()) => {}
            // Retried by `retry_new_tab_records`; an opening document wakes
            // the loop for it.
            Err(ui::record::RecordOpenError::Busy | ui::record::RecordOpenError::Opening) => {
                self.pending_tab_records
                    .push_back(PendingTabRecordCreation {
                        identity,
                        store: self.records.clone(),
                        checkout,
                    });
            }
            Err(error) => {
                self.failed_tab_records
                    .insert(tab, (identity, super::new_document_unavailable(&error)));
            }
        }
    }

    /// Retain every valid ticket, attempting at most 64 per drain. Busy tickets
    /// rotate behind unattempted work; rebuilding missing tabs adds no authority.
    pub(crate) fn retry_new_tab_records(&mut self) {
        let tree = self.tree_handle.read();
        let before = self.failed_tab_records.len();
        self.failed_tab_records.retain(|tab, (identity, _)| {
            TabRecordCreationIdentity::capture(&tree, *tab).as_ref() == Some(identity)
        });
        let mut changed = self.failed_tab_records.len() != before;
        // A document the store finished opening: tabs that were refused
        // while it opened bind again on the resync this bump causes.
        if self
            .records_opened
            .swap(false, std::sync::atomic::Ordering::Acquire)
        {
            changed = true;
        }
        self.pending_tab_records.retain(|pending| {
            TabRecordCreationIdentity::capture(&tree, pending.identity.tab).as_ref()
                == Some(&pending.identity)
        });
        let attempts = self.pending_tab_records.len().min(64);
        for _ in 0..attempts {
            let Some(pending) = self.pending_tab_records.pop_front() else {
                break;
            };
            match pending.store.create_record(&pending.checkout) {
                Ok(()) => changed = true,
                Err(ui::record::RecordOpenError::Busy | ui::record::RecordOpenError::Opening) => {
                    self.pending_tab_records.push_back(pending);
                }
                Err(error) => {
                    self.failed_tab_records.insert(
                        pending.identity.tab,
                        (pending.identity, super::new_document_unavailable(&error)),
                    );
                    changed = true;
                }
            }
        }
        drop(tree);
        if changed {
            self.bump();
        }
    }
}
