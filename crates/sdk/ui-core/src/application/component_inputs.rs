//! What a component at rest compares with what its last pass read ([`crate::component_inputs`]): its
//! record's save, and what its host still has to take from it.

use super::AppComponent;

impl AppComponent {
    /// The durability last delivered, as sampled: what the root was told of its record's save, which a
    /// component at rest compares with the store's.
    pub(crate) fn sampled_record_save(&self) -> Option<&crate::record::RecordSaveStatus> {
        self.record_save.as_ref()
    }

    /// The record's save status as a sync would sample it now: a save still being written arms its
    /// progress wake first, as the sync's sample does, since some stores move a save on only once asked
    /// to watch it ([`crate::record::RecordStore::wake_on_save_progress`]).
    pub(crate) fn record_save_now(&self) -> Option<crate::record::RecordSaveStatus> {
        let binding = self.callback_binding();
        if self.record_save == Some(crate::record::RecordSaveStatus::Saving)
            && let Some(wake) = &self.save_wake
        {
            binding
                .records
                .wake_on_save_progress(&binding.checkout.id, std::sync::Arc::clone(wake));
        }
        binding.records.save_status(&binding.checkout.id).ok()
    }

    /// Whether a replacement waits for the host to take it.
    #[cfg(debug_assertions)]
    pub(crate) fn replacement_waits(&self) -> bool {
        self.context.replacement.borrow().is_some()
    }
}
