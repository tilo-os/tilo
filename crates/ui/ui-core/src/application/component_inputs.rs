//! What a component at rest compares with what its last pass read ([`tilo_ui_base::component_inputs`]): its
//! record's save, and what its host still has to take from it.

use super::AppComponent;

impl AppComponent {
    /// The durability last delivered, as sampled: what the root was told of its record's save, which a
    /// component at rest compares with the store's.
    pub(crate) fn sampled_resource_save(&self) -> Option<&crate::resource::ResourceSaveStatus> {
        self.record_save.as_ref()
    }

    /// The save status the window shows, as a sync would sample it now: a save still being written
    /// arms its progress wake first, as the sync's sample does, since some stores move a save on only
    /// once asked to watch it ([`crate::resource::ResourceStore::wake_on_save_progress`]).
    pub(crate) fn record_save_now(&self) -> Option<crate::resource::ResourceSaveStatus> {
        let binding = self.callback_binding();
        let parts = binding.chain_parts();
        if let Some(wake) = &self.save_wake {
            binding.arm_save_progress(&parts, wake);
        }
        binding.shown_save_status(&parts).ok()
    }

    /// Whether a replacement waits for the host to take it.
    #[cfg(debug_assertions)]
    pub(crate) fn replacement_waits(&self) -> bool {
        self.context.replacement.borrow().is_some()
    }
}
