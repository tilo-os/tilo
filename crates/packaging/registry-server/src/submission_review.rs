//! Shared local operator workflow: verify the submitted bytes before staging
//! the exact reviewed metadata and making approved uploads available.

use crate::{
    InspectedVersion, InspectionPolicy, ModerationService, PendingSubmission, ReviewRecord, Storage,
};

/// Inspect the packages behind a pending request. Deletion has no new bytes.
///
/// # Errors
/// Returns the existing bounded download and signed package validation errors.
pub fn inspect(
    storage: &Storage,
    pending: &PendingSubmission,
    policy: InspectionPolicy,
) -> anyhow::Result<Vec<(InspectedVersion, Vec<u8>)>> {
    if pending.change.action == registry_types::EntryAction::Delete {
        return Ok(Vec::new());
    }
    Ok(crate::review_entry_bytes(
        storage,
        &pending.change.entry,
        policy,
    )?)
}

/// Reinspect bytes and approve the exact candidate using the existing locked
/// moderation transition. Uploaded artifacts become public only after approval.
/// This stages metadata; publishing a signed snapshot remains a separate step.
///
/// # Errors
/// Returns inspection, changed-request, authorization or storage errors.
pub fn approve(
    storage: &Storage,
    pending: &PendingSubmission,
    reviewer: &str,
    reason: &str,
    reviewed_at_unix: u64,
    policy: InspectionPolicy,
) -> anyhow::Result<ReviewRecord> {
    if storage.load_pending(&pending.submission_id)?.as_ref() != Some(pending) {
        return Err(crate::ModerationError::PendingChanged(pending.submission_id.clone()).into());
    }
    let packages = inspect(storage, pending, policy)?;
    let allowlist = storage.load_allowlist()?;
    let record = ModerationService::new(storage.clone()).approve(
        pending,
        reviewer,
        reason,
        reviewed_at_unix,
        &allowlist,
    )?;
    for (_inspection, bytes) in packages {
        storage.save_payload_blobs(&bytes)?;
        let digest = blake3::hash(&bytes).to_hex().to_string();
        if storage.load_upload(&digest)?.is_some() {
            storage.import_artifact(&bytes)?;
        }
    }
    Ok(record)
}
