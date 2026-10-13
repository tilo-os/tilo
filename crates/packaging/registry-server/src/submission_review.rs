//! Shared local operator workflow: verify the submitted bytes before staging
//! the exact reviewed metadata and making approved uploads available.

use serde::Serialize;

use crate::bot_review::{InspectedProgram, review_bot_versions};
use crate::{
    InspectedVersion, InspectionPolicy, ModerationService, PendingSubmission, ReviewRecord, Storage,
};

/// What review verified for one version: a package's bytes or a bot's program.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "artifact", rename_all = "kebab-case")]
pub enum Inspection {
    Package(InspectedVersion),
    Program(InspectedProgram),
}

/// Inspect the packages or programs behind a pending request. Deletion has
/// no new bytes.
///
/// # Errors
/// Returns the bounded download, signed package and bot program validation
/// errors.
pub fn inspect(
    storage: &Storage,
    pending: &PendingSubmission,
    policy: InspectionPolicy,
) -> anyhow::Result<Vec<(Inspection, Vec<u8>)>> {
    if pending.change.action == registry_types::EntryAction::Delete {
        return Ok(Vec::new());
    }
    let entry = &pending.change.entry;
    Ok(match entry.kind {
        registry_types::EntryKind::App => crate::review_entry_bytes(storage, entry, policy)?
            .into_iter()
            .map(|(inspected, bytes)| (Inspection::Package(inspected), bytes))
            .collect(),
        registry_types::EntryKind::Bot => review_bot_versions(storage, entry)?
            .into_iter()
            .map(|(inspected, bytes)| (Inspection::Program(inspected), bytes))
            .collect(),
    })
}

/// Reinspect bytes and approve the exact candidate using the existing locked
/// moderation transition. Uploaded packages and programs become public only
/// after approval. This stages metadata; publishing a signed snapshot
/// remains a separate step.
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
    let inspected = inspect(storage, pending, policy)?;
    let allowlist = storage.load_allowlist()?;
    let record = ModerationService::new(storage.clone()).approve(
        pending,
        reviewer,
        reason,
        reviewed_at_unix,
        &allowlist,
    )?;
    for (inspection, bytes) in inspected {
        match inspection {
            Inspection::Package(_) => {
                storage.save_payload_blobs(&bytes)?;
                let digest = blake3::hash(&bytes).to_hex().to_string();
                if storage.load_upload(&digest)?.is_some() {
                    storage.import_artifact(&bytes)?;
                }
            }
            Inspection::Program(_) => {
                storage.save_program_blob(&bytes)?;
            }
        }
    }
    Ok(record)
}
