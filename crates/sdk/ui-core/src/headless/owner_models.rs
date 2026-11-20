//! Serving the owner models a worker woke: one batch, its save-status work
//! shown to the owner that shows it, and the layout a service retires
//! (headless.rs keeps the report and books the frame).

use super::FrameHarness;

impl FrameHarness {
    /// Service `queue`'s front batch: a batch of save progress tells the owner
    /// each status it has not shown yet; any other batch is served through the
    /// retained tree, retiring it for readmission first.
    pub(super) fn service_owner_model_batch(
        &mut self,
        attempted: &mut bool,
        presented: &mut bool,
        completed: &mut usize,
    ) -> Result<(), crate::bound_dispatch::BoundDispatchError> {
        let _running = self.running();
        let Some(queue) = self.owner_wake_queue.clone() else {
            return Ok(());
        };
        let mut batch = queue.take_batch();
        if batch.front().is_some() && batch.all_save_progress_only() {
            while let Some(work) = batch.front() {
                let _profile =
                    tilo_resource::profiling::Phase::start("ui.owner_wake.save_progress");
                let owner = work.registration();
                if let Some(status) = crate::bound_dispatch::service_owner_save_progress(
                    self.root.as_mut(),
                    work,
                    self.save_status_told.get(&owner),
                )? {
                    self.save_status_told.insert(owner, status);
                    *presented = true;
                }
                batch.complete_front();
                *completed += 1;
            }
            return Ok(());
        }
        let mut aside = None;
        if batch.front().is_some() {
            // An unknown visitor can replace descendants before it finds the target, so the
            // physical maps are retired before the first arbitrary callback and kept for
            // readmission to check.
            *attempted = true;
            aside = self.set_layout_aside();
            self.last_layout_viewport = None;
            self.owner_service_layout_retired = true;
        }
        while let Some(work) = batch.front() {
            let _profile = tilo_resource::profiling::Phase::start("ui.owner_wake.service");
            crate::bound_dispatch::service_retained_owner_model(
                self.root.as_mut(),
                &mut self.state.doc,
                work,
            )?;
            batch.complete_front();
            *completed += 1;
        }
        // Only a service whose every callback returned keeps them; an error
        // or an unwind drops them with this frame.
        self.serviced_layout = aside;
        Ok(())
    }
}
