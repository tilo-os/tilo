use host_fs::attributes as xattr;
use std::path::Path;

use crate::i18n::{arg, t, t_args};

/// Request restoration through the original filesystem history-control
/// attribute. New UI flows use the checked review/restore transport, but this
/// entry point remains for applications compiled against the stable facade.
///
/// # Errors
///
/// Returns a localized error when the bounded request cannot be encoded or
/// written to the file's history-control extended attribute.
#[deprecated(note = "use the checked file-history transport for reviewed restores")]
pub fn restore_version(path: &Path, version_id: collab_types::FileVersionId) -> Result<(), String> {
    let request = collab_types::HistoryControlRequestV1 {
        schema_version: collab_types::HISTORY_CONTROL_VERSION,
        operation: collab_types::HistoryControlOperationV1::Restore { version_id },
    };
    let bytes = serde_json::to_vec(&request).map_err(|_| t("files-history-encode-failed"))?;
    if bytes.len() > collab_types::MAX_HISTORY_CONTROL_BYTES {
        return Err(t("files-history-request-too-large"));
    }
    xattr::set(path, crate::model::HISTORY_CONTROL_XATTR, &bytes).map_err(|error| {
        t_args(
            "files-history-restore-failed",
            &[arg("error", error.to_string())],
        )
    })
}

/// Request resolution of one exact conflict member using `strategy`.
///
/// # Errors
///
/// Returns a localized error for an unsupported strategy, an encoding or size
/// failure, or an extended-attribute write failure.
pub fn resolve_conflict(
    path: &Path,
    conflict_id: uuid::Uuid,
    strategy: collab_types::ConflictStrategy,
    version_id: collab_types::FileVersionId,
) -> Result<(), String> {
    if !matches!(
        strategy,
        collab_types::ConflictStrategy::KeepCurrent
            | collab_types::ConflictStrategy::UseOther
            | collab_types::ConflictStrategy::KeepBoth
            | collab_types::ConflictStrategy::Merge
    ) {
        return Err(t("files-conflict-strategy-unsupported"));
    }
    let request = collab_types::HistoryControlRequestV1 {
        schema_version: collab_types::HISTORY_CONTROL_VERSION,
        operation: collab_types::HistoryControlOperationV1::ResolveConflict {
            conflict_id,
            strategy,
            version_id,
        },
    };
    let bytes = serde_json::to_vec(&request).map_err(|_| t("files-conflict-encode-failed"))?;
    if bytes.len() > collab_types::MAX_HISTORY_CONTROL_BYTES {
        return Err(t("files-conflict-request-too-large"));
    }
    xattr::set(path, crate::model::HISTORY_CONTROL_XATTR, &bytes).map_err(|error| {
        t_args(
            "files-conflict-resolve-failed",
            &[arg("error", error.to_string())],
        )
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Restore and conflict requests are versioned, bounded and name the exact member for every strategy.
    #[test]
    fn history_requests_are_versioned_bounded_and_exact() {
        // --- phase: restore_request_is_versioned_bounded_and_exact ---
        {
            use collab_types::files::*;
            use ui::history_mode::checked_files::FileHistoryTransport;

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("record");
            std::fs::write(&path, b"").unwrap();
            let version_id = collab_types::FileVersionId(uuid::Uuid::new_v4());
            let request = CheckedHistoryRequestV1 {
                schema_version: CHECKED_HISTORY_VERSION,
                intent_id: uuid::Uuid::new_v4(),
                review: FileRestoreReviewV1 {
                    schema_version: CHECKED_HISTORY_VERSION,
                    workspace_id: uuid::Uuid::new_v4(),
                    mount_lifetime: uuid::Uuid::new_v4(),
                    record_id: collab_types::FileRecordId(uuid::Uuid::new_v4()),
                    frontier: [1; 32],
                },
                operation: CheckedHistoryOperationV1::Restore { version_id },
            };
            let response = CheckedHistoryResponseV1 {
                schema_version: CHECKED_HISTORY_VERSION,
                outcome: CheckedHistoryOutcomeV1::RecoveryPending {
                    intent_id: request.intent_id,
                },
            };
            xattr::set(
                &path,
                format!("{CHECKED_HISTORY_RESULT_XATTR_PREFIX}{}", request.intent_id),
                &serde_json::to_vec(&response).unwrap(),
            )
            .unwrap();
            let transport = crate::framework_history::MountedFileHistory::open(&path).unwrap();
            assert_eq!(transport.submit(request), response.outcome);
            let bytes = xattr::get(&path, CHECKED_HISTORY_RESTORE_XATTR)
                .unwrap()
                .unwrap();
            assert!(bytes.len() <= MAX_CHECKED_HISTORY_REQUEST_BYTES);
            let actual: CheckedHistoryRequestV1 = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(actual.schema_version, CHECKED_HISTORY_VERSION);
            assert_eq!(
                actual.operation,
                CheckedHistoryOperationV1::Restore { version_id }
            );
            assert_eq!(actual, request);
        }

        // --- phase: conflict_request_names_the_exact_member_for_every_supported_strategy ---
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("record");
            std::fs::write(&path, b"").unwrap();
            let conflict_id = uuid::Uuid::new_v4();
            let version_id = collab_types::FileVersionId::new();
            resolve_conflict(
                &path,
                conflict_id,
                collab_types::ConflictStrategy::UseOther,
                version_id,
            )
            .unwrap();
            let bytes = xattr::get(&path, crate::model::HISTORY_CONTROL_XATTR)
                .unwrap()
                .unwrap();
            let request: collab_types::HistoryControlRequestV1 =
                serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                request.operation,
                collab_types::HistoryControlOperationV1::ResolveConflict {
                    conflict_id,
                    strategy: collab_types::ConflictStrategy::UseOther,
                    version_id,
                }
            );
            resolve_conflict(
                &path,
                conflict_id,
                collab_types::ConflictStrategy::Merge,
                version_id,
            )
            .unwrap();
            let merge: collab_types::HistoryControlRequestV1 = serde_json::from_slice(
                &xattr::get(&path, crate::model::HISTORY_CONTROL_XATTR)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                merge.operation,
                collab_types::HistoryControlOperationV1::ResolveConflict {
                    conflict_id,
                    strategy: collab_types::ConflictStrategy::Merge,
                    version_id,
                }
            );
            resolve_conflict(
                &path,
                conflict_id,
                collab_types::ConflictStrategy::KeepBoth,
                version_id,
            )
            .unwrap();
        }
    }
}
