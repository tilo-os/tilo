use super::{AtomicSavePreparation, AtomicSaveReservation};

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
        let deadline = tilo_test_env::hang_deadline();
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
