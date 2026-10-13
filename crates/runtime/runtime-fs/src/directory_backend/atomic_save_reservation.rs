//! Ownership guard only. No state grants source rights or proves native stop.
use super::AtomicSavePreparation;

pub struct AtomicSaveReservation<T> {
    state: State<T>,
}
enum State<T> {
    Unknown,
    Prepared(T),
    Claimed,
    Released,
    CleanupSubmitted,
}
impl<T> Default for AtomicSaveReservation<T> {
    fn default() -> Self {
        Self {
            state: State::Unknown,
        }
    }
}
impl<T> AtomicSaveReservation<T> {
    /// Store the actual returned owner before any fallible caller validation.
    /// # Errors
    /// Returns the provider's refusal or unknown-outcome error, retaining that state.
    pub fn capture(&mut self, outcome: AtomicSavePreparation<T>) -> Result<(), String> {
        match outcome {
            AtomicSavePreparation::Prepared(owner) => {
                self.state = State::Prepared(owner);
                Ok(())
            }
            AtomicSavePreparation::Refused(error) => {
                self.state = State::Released;
                Err(error)
            }
            AtomicSavePreparation::Unknown(error) => {
                self.state = State::Unknown;
                Err(error)
            }
        }
    }
    /// # Errors
    /// Refuses reservations that are not currently prepared.
    pub fn prepared(&self) -> Result<&T, String> {
        match &self.state {
            State::Prepared(owner) => Ok(owner),
            _ => Err("AtomicSave reservation unavailable".into()),
        }
    }
    /// Failure retains the actual reservation; cancellation remains possible.
    /// # Errors
    /// Refuses unprepared reservations or propagates the caller's validation error.
    pub fn validate(&self, check: impl FnOnce(&T) -> Result<(), String>) -> Result<(), String> {
        check(self.prepared()?)
    }
    /// # Errors
    /// Refuses reservations that are not currently prepared.
    pub fn claim(&mut self) -> Result<T, String>
    where
        T: Clone,
    {
        let owner = self.prepared()?.clone();
        self.state = State::Claimed;
        Ok(owner)
    }
    /// Only actual cancel ACK releases an unsubmitted reservation. Failed or
    /// poisoned cancellation keeps its owner; unknown/claimed effects refuse.
    /// # Errors
    /// Propagates cancellation failures or refuses unresolved reservation custody.
    pub fn release_with(
        &mut self,
        cancel: impl FnOnce(&T) -> Result<(), String>,
    ) -> Result<(), String> {
        match &self.state {
            State::Prepared(owner) => {
                cancel(owner)?;
                self.state = State::Released;
                Ok(())
            }
            State::Released => Ok(()),
            _ => Err("AtomicSave reservation custody unresolved".into()),
        }
    }
    /// Called after an actual creator cleanup job is returned. This transition
    /// cannot prove Finished, release a slot, or authorize any namespace effect.
    /// # Errors
    /// Refuses cleanup submission until the reservation has been released.
    pub fn cleanup_submitted(&mut self) -> Result<(), String> {
        if !matches!(self.state, State::Released) {
            return Err("AtomicSave cleanup not released".into());
        }
        self.state = State::CleanupSubmitted;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory::DirectoryCapability;
    use crate::directory_backend::{JobUpdate, WorkspacePendingReplacementBackend};
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    type Pending = Arc<dyn WorkspacePendingReplacementBackend>;

    // Genuine generic zero-effect native reservation. These tests establish
    // custody only and do NOT manufacture selected AtomicSave admission.
    #[test]
    fn actual_pending_survives_injected_post_return_validation_failures() {
        for stage in ["check", "observe", "stat"] {
            let mut fixture = crate::operations::tests::pending_replacement_for_transport();
            let directory =
                DirectoryCapability::new(fixture.session.clone(), fixture.parent.clone()).unwrap();
            let pending = directory
                .pending_for_transport_test(fixture.prepared.take().unwrap())
                .unwrap();
            let mut guard = AtomicSaveReservation::default();
            guard
                .capture(AtomicSavePreparation::Prepared(pending.clone()))
                .unwrap();
            assert!(
                guard
                    .validate(|owner| {
                        owner.check()?;
                        owner.observe()?.validate()?;
                        Err(format!("injected {stage} post-return refusal"))
                    })
                    .is_err()
            );
            assert!(Arc::ptr_eq(guard.prepared().unwrap(), &pending));
            fixture.assert_original_entries();
            guard.release_with(|owner| owner.cancel()).unwrap();
            assert!(
                pending.check().is_err(),
                "actual zero-effect work was released"
            );
            fixture.assert_original_entries();
        }
    }
    #[test]
    fn failed_actual_cancel_and_claimed_reservation_never_allow_cleanup() {
        let mut fixture = crate::operations::tests::pending_replacement_for_transport();
        let directory =
            DirectoryCapability::new(fixture.session.clone(), fixture.parent.clone()).unwrap();
        let pending = directory
            .poisoned_pending_for_transport_test(fixture.prepared.take().unwrap())
            .unwrap();
        let mut guard = AtomicSaveReservation::default();
        guard
            .capture(AtomicSavePreparation::Prepared(pending.clone()))
            .unwrap();
        assert!(guard.release_with(|owner| owner.cancel()).is_err());
        assert!(Arc::ptr_eq(guard.prepared().unwrap(), &pending));
        assert!(guard.cleanup_submitted().is_err());
        let _claimed = guard.claim().unwrap();
        assert!(guard.release_with(|owner| owner.cancel()).is_err());
        fixture.assert_original_entries();
    }
    #[test]
    fn actual_unsubmitted_cancel_after_revocation_releases_reservation() {
        let mut fixture = crate::operations::tests::pending_replacement_for_transport();
        let directory =
            DirectoryCapability::new(fixture.session.clone(), fixture.parent.clone()).unwrap();
        let pending = directory
            .pending_for_transport_test(fixture.prepared.take().unwrap())
            .unwrap();
        let mut guard = AtomicSaveReservation::default();
        guard
            .capture(AtomicSavePreparation::Prepared(pending.clone()))
            .unwrap();
        fixture.revoke().unwrap();
        assert!(pending.check().is_err(), "data authority was revoked");
        // No-effects cancellation deliberately uses ownership, not current
        // data authority. Its real ACK releases this actual prepared work.
        guard.release_with(|owner| owner.cancel()).unwrap();
        assert!(guard.prepared().is_err());
        fixture.assert_original_entries();
    }
    #[test]
    fn actual_refused_creator_can_retire_after_known_cleanup_admission_retry() {
        let mut fixture = crate::operations::tests::completed_created_temporary_for_transport();
        let directory =
            DirectoryCapability::new(fixture.session.clone(), fixture.parent.clone()).unwrap();
        let job = directory
            .job_for_transport_test(fixture.job.take().unwrap())
            .unwrap();
        let creator = job.created_temporary().unwrap();
        creator.check().unwrap();
        let mut guard: AtomicSaveReservation<Pending> = AtomicSaveReservation::default();
        let outcome = creator.prepare_atomic_save();
        assert!(
            matches!(&outcome, AtomicSavePreparation::Refused(_)),
            "no sealed selected AtomicSave permission exists"
        );
        assert!(guard.capture(outcome).is_err());
        guard.release_with(|owner| owner.cancel()).unwrap();
        // A pre-submission admission fault changes no guard state. Retrying
        // uses the same real creator, whose native one-use gate remains live.
        let admission: Result<(), String> = Err("injected cleanup admission refusal".into());
        assert!(admission.is_err());
        guard.release_with(|owner| owner.cancel()).unwrap();
        let cleanup = creator.retire().unwrap();
        guard.cleanup_submitted().unwrap();
        assert!(
            creator.retire().is_err(),
            "real creator cannot submit twice"
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut captured = false;
        loop {
            let poll = cleanup.poll().unwrap();
            let mut finished = false;
            for update in poll.updates {
                match update {
                    JobUpdate::Item {
                        state: crate::operations::ItemState::Completed,
                        destination: None,
                        error: None,
                        recoveries,
                        ..
                    } => {
                        assert!(!recoveries.is_empty());
                        captured = true;
                    }
                    JobUpdate::Finished {
                        completed: 1,
                        failed: 0,
                        cancelled: 0,
                    } => {
                        assert!(captured);
                        finished = true;
                    }
                    JobUpdate::Progress { .. } => {}
                    _ => panic!("unexpected actual cleanup outcome"),
                }
            }
            if finished {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "actual native cleanup did not complete"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        fixture.assert_retired_recoverably();
    }
    #[test]
    fn unknown_prepare_keeps_actual_creator_and_forbids_cleanup_transition() {
        let mut fixture = crate::operations::tests::completed_created_temporary_for_transport();
        let directory =
            DirectoryCapability::new(fixture.session.clone(), fixture.parent.clone()).unwrap();
        let job = directory
            .job_for_transport_test(fixture.job.take().unwrap())
            .unwrap();
        let creator = job.created_temporary().unwrap();
        let mut guard: AtomicSaveReservation<Pending> = AtomicSaveReservation::default();
        assert!(
            guard
                .capture(AtomicSavePreparation::Unknown(
                    "injected transport uncertainty".into()
                ))
                .is_err()
        );
        assert!(guard.release_with(|owner| owner.cancel()).is_err());
        assert!(guard.claim().is_err());
        assert!(guard.cleanup_submitted().is_err());
        creator.check().unwrap();
        fixture.assert_original_entries();
    }
}
