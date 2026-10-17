//! Canonical root-session completion of an authenticated pending original migration.
//! A verified Home receipt alone never reports completed setup.
use super::*;
use crate::{
    imager_model::TargetDrive,
    setup::{
        home_capture::RootMediaReadSession, media_setup::boot_review::PendingBootReview, migration,
    },
};
use std::{thread, time::Duration};
use zeroize::Zeroizing;

/// The same authenticated read helper keeps observing peer/cancel retirement
/// while its owned worker retains raw/media/source custody through final cleanup.
pub fn complete_pending_boot(
    target: &TargetDrive,
    raw: &File,
    caller: u32,
    password: Zeroizing<Vec<u8>>,
    cancel: &AtomicBool,
    retirement: &mut RootMediaReadSession,
) -> io::Result<()> {
    if caller == 0 || password.is_empty() || password.len() > 4096 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    retirement.check_boot_cancel(cancel)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation = ServiceCancellation::default();
    let worker_cancelled = cancelled.clone();
    let worker_cancellation = cancellation.clone();
    let target = target.clone();
    let raw = raw.try_clone()?;
    thread::scope(|scope| {
        let job = scope.spawn(move || {
            crate::write::with_locked_media_root(&target, || {
                complete_owned(
                    &target,
                    &raw,
                    caller,
                    password,
                    &worker_cancelled,
                    worker_cancellation,
                )
            })
        });
        let mut observed_error = None;
        while !job.is_finished() {
            if let Err(error) = retirement.check_boot_cancel(cancel) {
                cancelled.store(true, std::sync::atomic::Ordering::Release);
                cancellation.cancel();
                if observed_error.is_none() {
                    observed_error = Some(error);
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
        let result = job
            .join()
            .map_err(|_| refused("Original completion worker panicked"))?;
        // Never let cancelled peer retirement become a success acknowledgement,
        // even if a durable commit completed just before cancellation arrived.
        if let Some(error) = observed_error {
            return Err(error);
        }
        retirement.check_boot_cancel(cancel)?;
        result
    })
}
/// Bounded data copied from real quiescent/verified production transactions.
/// Construction remains private; no serialized fixture claim can mint this.
pub(in crate::setup::home_capture) struct CompletedQualificationEvidence {
    pub(in crate::setup::home_capture) source_disk_guids: Vec<String>,
    pub(in crate::setup::home_capture) read_claim_sha256: [u8; 32],
    pub(in crate::setup::home_capture) home: crate::setup::portable_home_media::Expectation,
    pub(in crate::setup::home_capture) applications:
        crate::setup::flatpak_transfer::portable::Expectation,
    pub(in crate::setup::home_capture) completion_receipt_sha256: [u8; 32],
}
fn complete_owned(
    target: &TargetDrive,
    raw: &File,
    caller: u32,
    password: Zeroizing<Vec<u8>>,
    cancel: &AtomicBool,
    cancellation: ServiceCancellation,
) -> io::Result<()> {
    complete_owned_run(target, raw, caller, password, cancel, cancellation, false).map(|_| ())
}
/// Only the fixed installed consumer qualifier uses this internal entry. Normal
/// pending completion keeps its existing retry/caller behavior unchanged.
pub(in crate::setup::home_capture) fn qualify_owned(
    target: &TargetDrive,
    raw: &File,
    password: Zeroizing<Vec<u8>>,
    cancel: &AtomicBool,
    cancellation: ServiceCancellation,
) -> io::Result<CompletedQualificationEvidence> {
    complete_owned_run(target, raw, 1000, password, cancel, cancellation, true)?
        .ok_or_else(|| refused("Qualification did not execute the actual original worker"))
}
fn complete_owned_run(
    target: &TargetDrive,
    raw: &File,
    caller: u32,
    password: Zeroizing<Vec<u8>>,
    cancel: &AtomicBool,
    cancellation: ServiceCancellation,
    qualification: bool,
) -> io::Result<Option<CompletedQualificationEvidence>> {
    use sha2::Digest as _;
    if caller == 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    // The authenticated boot administrator and the protected original source
    // user are distinct identities. Password/setup and source proofs authorize
    // the transfer; the receiver UID never substitutes for original credentials.
    migration::check_cancel(cancel)?;
    let read = with_boot_medium(target, raw, cancel, |medium| {
        if let Some(committed) = crate::setup::prepared_setup::completion::boot_review::CompletedBootReview::review_if_committed(medium, &password, cancel)? {
            // The actual selected Store, completion marker and both sealed
            // generations authenticate retry; no source export is restarted.
            if qualification { return Err(refused("Qualification requires fresh actual original export, not committed retry")); }
            committed.validate(cancel)?;
            drop(committed);
            return Ok(None);
        }
        let pending = PendingBootReview::review(medium, &password, cancel)?;
        let read = OfflineReadAdmission::review_boot_pending(
            &pending,
            raw.try_clone()?,
            target.clone(),
            cancel,
        )?;
        pending.validate()?;
        Ok(Some(Arc::new(read)))
    })?;
    let Some(read) = read else {
        // Existing completion is acknowledged only after readonly retirement;
        // the controller still reacquires and adopts the exact generation.
        migration::check_cancel(cancel)?;
        return Ok(None);
    };
    // The original readonly medium mount has actually retired before this
    // independently source-disjoint writable output mount can be admitted.
    migration::check_cancel(cancel)?;
    if read
        .output_selection_declaration(cancel)?
        .is_applications_only()
    {
        if qualification {
            return Err(refused(
                "Worker5 qualification cannot certify applications-only migration",
            ));
        }
        complete_applications_read(read, password, cancel, cancellation)?;
        return Ok(None);
    }
    let qualified_sources = if qualification {
        Some(read.qualification_owned_sources(cancel)?)
    } else {
        None
    };
    let read_claim = if qualification {
        Some(read.output_read_claim_digest(cancel)?)
    } else {
        None
    };
    let (transaction, medium) = OfflineOutputTransaction::mount(read.clone(), cancellation)?;
    let outcome = transaction.export_and_verify(Zeroizing::new(password.to_vec()));
    let result = (|| {
        outcome
            .result
            .as_ref()
            .map_err(|_| refused("Original export or verifier failed"))?;
        migration::check_cancel(cancel)?;
        let verified = transaction.validate_verified_outcome(&outcome)?;
        let setup = read.authenticated_setup();
        let mut package = setup.reseal_completed_home(&verified, password, cancel)?;
        let receipt = package.publish(&verified, cancel)?;
        if !receipt.setup.valid() {
            return Err(refused("Completed setup expectation differs"));
        }
        verified.validate()?;
        migration::check_cancel(cancel)?;
        let evidence = match (qualified_sources, read_claim) {
            (Some(source_disk_guids), Some(read_claim_sha256)) => {
                Some(CompletedQualificationEvidence {
                    source_disk_guids,
                    read_claim_sha256,
                    home: verified.expected().clone(),
                    applications: verified.applications().cloned().ok_or_else(|| {
                        refused("Qualification selected application export missing")
                    })?,
                    completion_receipt_sha256: sha2::Sha256::digest(serde_json::to_vec(&receipt)?)
                        .into(),
                })
            }
            (None, None) => None,
            _ => return Err(refused("Qualification source evidence differs")),
        };
        Ok(evidence)
    })();
    drop(transaction);
    drop(read);
    // Both successful and cancelled/failed workers require the real canonical
    // namespace-death receipt before root releases its owned writable mount.
    let (shutdown, key, work) = outcome.cleanup_receipt()?;
    medium.finish(shutdown, key, work)?;
    result
}

/// Result of the selected-material factory. No Home receipt is fabricated.
/// Only this module can construct it after actual independent validation and
/// observed source/output retirement.
pub(in crate::setup::home_capture) struct CompletedApplicationsQualificationEvidence {
    pub(in crate::setup::home_capture) setup: crate::imager_model::PreparedSetupExpectation,
    pub(in crate::setup::home_capture) source_disk_guids: Vec<String>,
    pub(in crate::setup::home_capture) read_claim_sha256: [u8; 32],
    pub(in crate::setup::home_capture) applications:
        crate::setup::flatpak_transfer::portable::Expectation,
    pub(in crate::setup::home_capture) completion_receipt_sha256: [u8; 32],
}
/// Fixed installed original6 factory; no caller paths, declarations or proof DTOs.
pub(in crate::setup::home_capture) fn qualify_applications_owned(
    target: &TargetDrive,
    raw: &File,
    password: Zeroizing<Vec<u8>>,
    cancel: &AtomicBool,
    cancellation: ServiceCancellation,
) -> io::Result<CompletedApplicationsQualificationEvidence> {
    migration::check_cancel(cancel)?;
    let read = with_boot_medium(target, raw, cancel, |medium| {
        if crate::setup::prepared_setup::completion::boot_review::CompletedBootReview::review_if_committed(medium, &password, cancel)?.is_some() {
            return Err(refused("Applications qualification requires a fresh actual export"));
        }
        let pending = PendingBootReview::review(medium, &password, cancel)?;
        let read = OfflineReadAdmission::review_boot_pending(
            &pending,
            raw.try_clone()?,
            target.clone(),
            cancel,
        )?;
        // The actual admission getter validates the private declaration;
        // this module does not widen the declaration's constructor/API.
        let _ = read.output_applications_declaration(cancel)?;
        pending.validate()?;
        Ok(Arc::new(read))
    })?;
    // All participant source roots must carry the same protected disposable
    // marker used by the existing Home factory. It supplies no source authority.
    let sources = read.qualification_owned_sources(cancel)?;
    let claim = read.output_read_claim_digest(cancel)?;
    complete_applications_run(read, password, cancel, cancellation, Some((sources, claim)))?
        .ok_or_else(|| refused("Actual applications qualification result missing"))
}
fn complete_applications_read(
    read: Arc<OfflineReadAdmission>,
    password: Zeroizing<Vec<u8>>,
    cancel: &AtomicBool,
    cancellation: ServiceCancellation,
) -> io::Result<()> {
    complete_applications_run(read, password, cancel, cancellation, None).map(|_| ())
}
fn complete_applications_run(
    read: Arc<OfflineReadAdmission>,
    password: Zeroizing<Vec<u8>>,
    cancel: &AtomicBool,
    cancellation: ServiceCancellation,
    qualification: Option<(Vec<String>, [u8; 32])>,
) -> io::Result<Option<CompletedApplicationsQualificationEvidence>> {
    use sha2::Digest as _;
    migration::check_cancel(cancel)?;
    let _ = read.output_applications_declaration(cancel)?;
    let (transaction, medium) = OfflineOutputTransaction::mount(read.clone(), cancellation)?;
    let outcome = transaction.export_applications_and_verify(Zeroizing::new(password.to_vec()));
    let result = (|| {
        outcome
            .result
            .as_ref()
            .map_err(|_| refused("Applications export or verifier failed"))?;
        migration::check_cancel(cancel)?;
        let verified = transaction.validate_verified_applications_outcome(&outcome)?;
        let setup = read.authenticated_setup();
        let mut package = setup.reseal_completed_applications(&verified, password, cancel)?;
        let receipt = package.publish_applications(&verified, cancel)?;
        if !receipt.setup.valid() {
            return Err(refused("Applications completed setup expectation differs"));
        }
        verified.validate()?;
        migration::check_cancel(cancel)?;
        qualification
            .map(|(source_disk_guids, read_claim_sha256)| {
                Ok(CompletedApplicationsQualificationEvidence {
                    setup: receipt.setup.clone(),
                    source_disk_guids,
                    read_claim_sha256,
                    applications: verified.expected().clone(),
                    completion_receipt_sha256: sha2::Sha256::digest(serde_json::to_vec(&receipt)?)
                        .into(),
                })
            })
            .transpose()
    })();
    drop(transaction);
    drop(read);
    // Cleanup never substitutes an outer wait for actual namespace retirement.
    let (shutdown, key, work) = outcome.cleanup_receipt()?;
    medium.finish(shutdown, key, work)?;
    result
}

/// Authenticate and adopt the fixed standard installer generation only inside
/// the exact selected readonly medium scope. Success follows owned retirement.
pub fn extract_completed_boot(
    target: &TargetDrive,
    raw: &File,
    caller: u32,
    password: Zeroizing<Vec<u8>>,
    cancel: &AtomicBool,
    retirement: &mut RootMediaReadSession,
) -> io::Result<()> {
    if caller == 0 || password.is_empty() || password.len() > 4096 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    retirement.check_boot_cancel(cancel)?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = cancelled.clone();
    let target = target.clone();
    let raw = raw.try_clone()?;
    thread::scope(|scope| {
        let job = scope.spawn(move || {
            crate::write::with_locked_media_root(&target, || {
                migration::check_cancel(&worker_cancelled)?;
                with_boot_medium(&target, &raw, &worker_cancelled, |medium| {
                    let review = crate::setup::prepared_setup::completion::boot_review::CompletedBootReview::review(
                        medium, &password, &worker_cancelled,
                    )?;
                    migration::check_cancel(&worker_cancelled)?;
                    review.adopt_install_generation(&worker_cancelled)?;
                    review.validate(&worker_cancelled)?;
                    // All borrowed source pins die before with_boot_medium's
                    // confirmed owned readonly unmount, including error paths.
                    drop(review);
                    Ok(())
                })
            })
        });
        let mut observed_error = None;
        while !job.is_finished() {
            if let Err(error) = retirement.check_boot_cancel(cancel) {
                cancelled.store(true, std::sync::atomic::Ordering::Release);
                if observed_error.is_none() {
                    observed_error = Some(error);
                }
            }
            thread::sleep(Duration::from_millis(25));
        }
        let result = job
            .join()
            .map_err(|_| refused("Completed extraction worker panicked"))?;
        if let Some(error) = observed_error {
            return Err(error);
        }
        retirement.check_boot_cancel(cancel)?;
        result
    })
}
