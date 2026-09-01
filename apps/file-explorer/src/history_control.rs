use std::path::Path;

use crate::i18n::{arg, t, t_args};

/// Request restoration of one exact immutable version through the filesystem.
///
/// # Errors
///
/// Returns a localized error when the request cannot be encoded within its
/// bound or written to the history-control extended attribute.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_request_is_versioned_bounded_and_exact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record");
        std::fs::write(&path, b"").unwrap();
        let version_id = collab_types::FileVersionId(uuid::Uuid::new_v4());
        restore_version(&path, version_id).unwrap();
        let bytes = xattr::get(&path, crate::model::HISTORY_CONTROL_XATTR)
            .unwrap()
            .unwrap();
        assert!(bytes.len() <= collab_types::MAX_HISTORY_CONTROL_BYTES);
        let request: collab_types::HistoryControlRequestV1 =
            serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            request.schema_version,
            collab_types::HISTORY_CONTROL_VERSION
        );
        assert_eq!(
            request.operation,
            collab_types::HistoryControlOperationV1::Restore { version_id }
        );
    }

    #[test]
    fn conflict_request_names_the_exact_member_for_every_supported_strategy() {
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
