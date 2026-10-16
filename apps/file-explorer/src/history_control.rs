use host_fs::attributes as xattr;
use std::path::Path;

use crate::i18n::{arg, t, t_args};

/// Request resolution of one exact conflict member using `strategy`.
///
/// # Errors
///
/// Returns a localized error for an unsupported strategy, an encoding or size
/// failure, or an extended-attribute write failure.
pub fn resolve_conflict(
    path: &Path,
    conflict_id: uuid::Uuid,
    strategy: fs_protocol::ConflictStrategy,
    version_id: fs_protocol::FileVersionId,
) -> Result<(), String> {
    if !matches!(
        strategy,
        fs_protocol::ConflictStrategy::KeepCurrent
            | fs_protocol::ConflictStrategy::UseOther
            | fs_protocol::ConflictStrategy::KeepBoth
            | fs_protocol::ConflictStrategy::Merge
    ) {
        return Err(t("files-conflict-strategy-unsupported"));
    }
    let request = fs_protocol::HistoryControlRequestV1 {
        schema_version: fs_protocol::HISTORY_CONTROL_VERSION,
        operation: fs_protocol::HistoryControlOperationV1::ResolveConflict {
            conflict_id,
            strategy,
            version_id,
        },
    };
    let bytes = serde_json::to_vec(&request).map_err(|_| t("files-conflict-encode-failed"))?;
    if bytes.len() > fs_protocol::MAX_HISTORY_CONTROL_BYTES {
        return Err(t("files-conflict-request-too-large"));
    }
    crate::framework_history::write_until_hydrated(crate::framework_history::HYDRATION_WAIT, || {
        xattr::set(path, fs_protocol::HISTORY_CONTROL_XATTR, &bytes)
    })
    .map_err(|error| {
        t_args(
            "files-conflict-resolve-failed",
            &[arg("error", error.to_string())],
        )
    })
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    /// Restore and conflict requests are versioned, bounded and name the exact member for every strategy.
    #[test]
    fn history_requests_are_versioned_bounded_and_exact() {
        // --- phase: restore_request_is_versioned_bounded_and_exact ---
        {
            use fs_protocol::files::*;
            use ui::history_mode::checked_files::FileHistoryTransport;

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("record");
            std::fs::write(&path, b"").unwrap();
            let version_id = fs_protocol::FileVersionId(uuid::Uuid::new_v4());
            let request = CheckedHistoryRequestV1 {
                schema_version: CHECKED_HISTORY_VERSION,
                intent_id: uuid::Uuid::new_v4(),
                review: FileRestoreReviewV1 {
                    schema_version: CHECKED_HISTORY_VERSION,
                    workspace_id: uuid::Uuid::new_v4(),
                    mount_lifetime: uuid::Uuid::new_v4(),
                    record_id: fs_protocol::FileRecordId(uuid::Uuid::new_v4()),
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
            let version_id = fs_protocol::FileVersionId::new();
            resolve_conflict(
                &path,
                conflict_id,
                fs_protocol::ConflictStrategy::UseOther,
                version_id,
            )
            .unwrap();
            let bytes = xattr::get(&path, fs_protocol::HISTORY_CONTROL_XATTR)
                .unwrap()
                .unwrap();
            let request: fs_protocol::HistoryControlRequestV1 =
                serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                request.operation,
                fs_protocol::HistoryControlOperationV1::ResolveConflict {
                    conflict_id,
                    strategy: fs_protocol::ConflictStrategy::UseOther,
                    version_id,
                }
            );
            resolve_conflict(
                &path,
                conflict_id,
                fs_protocol::ConflictStrategy::Merge,
                version_id,
            )
            .unwrap();
            let merge: fs_protocol::HistoryControlRequestV1 = serde_json::from_slice(
                &xattr::get(&path, fs_protocol::HISTORY_CONTROL_XATTR)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                merge.operation,
                fs_protocol::HistoryControlOperationV1::ResolveConflict {
                    conflict_id,
                    strategy: fs_protocol::ConflictStrategy::Merge,
                    version_id,
                }
            );
            resolve_conflict(
                &path,
                conflict_id,
                fs_protocol::ConflictStrategy::KeepBoth,
                version_id,
            )
            .unwrap();
        }
    }
}
