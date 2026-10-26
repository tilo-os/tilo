//! Typed backend seam shared by native grant owners and guarded RPC relays.
//! These objects never encode credentials, groups, native grants or root claims.
use crate::workspace_server::WorkspaceFileBackend;
use crate::{EntryName, OpenMode};
use std::sync::Arc;
pub use tilo_fs_client::directory_types::*;

#[cfg(test)]
mod atomic_save_reservation;
/// Genuine source-resident Read custody. Implementations must retain actual
/// original credentials and grant; descriptive observations cannot construct it.
pub trait WorkspaceHistoryReadSubjectBackend: Send + Sync + 'static {
    /// Release only this private controller cache alias after its admitted owner
    /// observed retirement. No File/OFD or creator aliases are bulk-deleted.
    /// # Errors
    /// Refuses unsupported cache retirement or missing admitted ownership.
    fn retire_controller_cache(&self) -> Result<(), String> {
        Err("Owned controller cache retirement unavailable".into())
    }

    /// # Errors
    /// Refuses unsupported retirement, an invalid ticket or missing handoff ownership.
    fn retire_daemon_handoff(&self, _ticket: [u8; 16]) -> Result<(), String> {
        Err("History Read handoff retirement unavailable".into())
    }
    /// # Errors
    /// Refuses unsupported handoff, failed authentication or revoked Read custody.
    fn arm_daemon_handoff(&self, _challenge: [u8; 32]) -> Result<[u8; 16], String> {
        Err("History Read handoff unavailable".into())
    }

    /// # Errors
    /// Refuses revoked or unavailable original-source Read scope.
    fn check_scope(&self) -> Result<(), String>;
    /// # Errors
    /// Refuses stale inode custody or revoked original-source Read authority.
    fn check(&self) -> Result<(), String>;
    /// # Errors
    /// Refuses invalid original-source observations, revoked scope or native IO.
    fn observe(&self) -> Result<HistoryReadObservation, String>;
}
pub trait WorkspaceDirectoryBackend: Send + Sync + 'static {
    /// Source providers cannot infer canonical history from path/inode labels.
    /// # Errors
    /// Refuses absent independently enrolled Read/history authority.
    fn history_query(
        &self,
        _name: &EntryName,
        _expected: [u64; 2],
        _query: &[u8],
    ) -> Result<Vec<u8>, String> {
        Err("Enrolled original-source history Read unavailable".into())
    }

    /// # Errors
    /// Refuses unsupported providers, revoked Read scope or nonregular children.
    fn history_read_subject(
        &self,
        _name: &EntryName,
    ) -> Result<Arc<dyn WorkspaceHistoryReadSubjectBackend>, String> {
        Err("Original-source history Read custody unavailable".into())
    }

    /// # Errors
    /// Refuses unsupported creator authority, changed inode identity or native IO.
    fn set_created_permissions(
        &self,
        _name: &EntryName,
        _expected: [u64; 2],
        _mode: u32,
    ) -> Result<(), String> {
        Err("Own-created permissions authority unavailable".into())
    }

    /// Only a filtered own-created scope implements this. Ordinary Home cannot
    /// derive deletion/capture authority from caller names or inode labels.
    /// # Errors
    /// Refuses absent own-created custody, changed inode identity or revoked scope.
    fn retire_created(
        &self,
        _name: &EntryName,
        _expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        Err("Own-created transient custody unavailable".into())
    }

    /// # Errors
    /// Refuses unsupported providers, revoked metadata scope or actual fsync IO.
    fn sync_directory(&self) -> Result<(), String> {
        Err("Held source directory sync unavailable".into())
    }

    /// # Errors
    /// Refuses unsupported providers or revoked genuine directory metadata scope.
    fn prepare_replace(
        &self,
        temporary: &EntryName,
        temporary_expected: [u64; 2],
        victim: &EntryName,
        victim_expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspacePendingReplacementBackend>, String> {
        let _ = (temporary, temporary_expected, victim, victim_expected);
        Err("Certified replacing source unavailable".into())
    }
    /// # Errors
    /// Refuses unsupported providers or revoked genuine directory metadata scope.
    fn statfs(&self) -> Result<DirectoryFilesystemStats, String> {
        Err("Source filesystem capacity unavailable".into())
    }
    /// # Errors
    /// Refuses revoked or unavailable native/root consumption authority.
    fn check(&self) -> Result<(), String>;
    /// Open one independent native cursor retaining this genuine directory grant.
    /// # Errors
    /// Refuses native enumeration admission or revoked scope.
    fn open_cursor(&self) -> Result<Arc<dyn WorkspaceDirectoryCursorBackend>, String>;
    fn metadata(&self) -> Result<DirectoryStat, String>;
    /// # Errors
    /// Observes one child without following links or opening its bytes.
    fn stat(&self, name: &EntryName) -> Result<Option<DirectoryStat>, String>;
    /// Observe raw symlink bytes without granting target traversal or content access.
    /// # Errors
    /// Refuses substituted links, revoked scope, or native IO.
    fn readlink(&self, name: &EntryName, expected: [u64; 2]) -> Result<Vec<u8>, String>;
    /// Retain a regular inode for metadata only, without byte access.
    /// # Errors
    /// Refuses changed identity, wrong type, revoked scope or native IO.
    fn pin_metadata(
        &self,
        name: &EntryName,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceMetadataBackend>, String>;
    /// # Errors
    /// Refuses stale cursors, revoked scope, snapshot limits or host IO.
    fn list(&self, cursor: u64) -> Result<DirectoryRows, String>;
    fn directory(&self, name: &EntryName) -> Result<Arc<dyn WorkspaceDirectoryBackend>, String>;
    fn open_file(
        &self,
        name: &EntryName,
        mode: OpenMode,
        expected: [u64; 2],
    ) -> Result<Arc<dyn WorkspaceFileBackend>, String>;
    fn create_file(&self, name: EntryName) -> Result<Arc<dyn WorkspaceJobBackend>, String>;
    fn create_directory(&self, name: EntryName) -> Result<Arc<dyn WorkspaceJobBackend>, String>;
    /// # Errors
    /// Refuses missing private owner, foreign cursor or revoked parent scope.
    fn recoveries(&self, after: &[u8]) -> Result<RecoveryPage, String> {
        let _ = after;
        Err("Native recovery owner not admitted".into())
    }
    /// # Errors
    /// Refuses unrelated recovery, missing genuine grants, collision or revoked scope.
    fn restore_captured(
        &self,
        recovery: [u8; 16],
        name: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let _ = (recovery, name);
        Err("Native recovery owner not admitted".into())
    }
    fn atomic_save(
        &self,
        _temporary: &EntryName,
        _expected: [u64; 2],
        _target: &EntryName,
    ) -> Result<(), String> {
        Err("Explicit AtomicSave coordinator unavailable".into())
    }
    /// # Errors
    /// Refuses unavailable certified replacing authority, revoked scope or native IO.
    fn rename_replace(
        &self,
        _name: &EntryName,
        _new_name: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        Err("Certified replacing-save coordinator unavailable".into())
    }
    /// # Errors
    /// Refuses missing rename authority, destination collisions or native IO.
    fn rename_no_replace(
        &self,
        name: &EntryName,
        new_name: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String>;
}
/// A held regular inode with metadata access only. No byte methods exist.
pub trait WorkspaceMetadataBackend: Send + Sync + 'static {
    /// Native/guarded providers return Some for both valid and revoked scope.
    fn live_scope(&self) -> Option<Result<(), String>> {
        None
    }
    fn metadata(&self) -> Result<DirectoryStat, String>;
}
/// Real native durable outcomes (or their authenticated relay), not signed
/// collaborative file history. Neither record IDs nor updates confer authority.
/// The provider retains a genuine native pin; no bytes or ownership mutation.
pub trait WorkspaceReplacementPinBackend: Send + Sync + 'static {
    /// Release only this private controller cache alias after its admitted owner
    /// observed retirement. No File/OFD or creator aliases are bulk-deleted.
    /// # Errors
    /// Refuses unsupported cache retirement or missing admitted ownership.
    fn retire_controller_cache(&self) -> Result<(), String> {
        Err("Owned controller cache retirement unavailable".into())
    }

    /// # Errors
    /// Refuses unsupported commits, invalid history receipts or revoked pin custody.
    fn commit_with_history(&self, _receipt: &[u8]) -> Result<(), String> {
        Err("Certified durable history commit unavailable".into())
    }

    /// # Errors
    /// Refuses revoked source scope or changed capture/destination custody.
    fn check(&self) -> Result<(), String>;
    /// # Errors
    /// Reobserves the genuine pin under its current source/parent authority.
    fn observe(&self) -> Result<ReplacementObservation, String>;
}
pub trait WorkspaceJobBackend: Send + Sync + 'static {
    /// Release only this private controller cache alias after its admitted owner
    /// observed retirement. No File/OFD or creator aliases are bulk-deleted.
    /// # Errors
    /// Refuses unsupported cache retirement or missing admitted ownership.
    fn retire_controller_cache(&self) -> Result<(), String> {
        Err("Owned controller cache retirement unavailable".into())
    }

    /// # Errors
    /// Only the actual native completed creation can mint transient custody.
    fn created_temporary(&self) -> Result<Arc<dyn WorkspaceCreatedTemporaryBackend>, String> {
        Err("Actual created temporary custody unavailable".into())
    }

    /// # Errors
    /// Refuses ordinary/uncertain jobs, absent root source binding or revoked scope.
    fn pin_replacement(&self) -> Result<Arc<dyn WorkspaceReplacementPinBackend>, String> {
        Err("Completed replacement custody unavailable".into())
    }
    /// Native/guarded providers return Some for both valid and revoked scope.
    fn live_scope(&self) -> Option<Result<(), String>> {
        None
    }
    fn poll(&self) -> Result<JobPoll, String>;
    /// # Errors
    /// Refuses revoked scope or native/relay cancellation failures.
    fn cancel(&self) -> Result<(), String>;
}

/// An independent held-directory cursor, with no byte or mutation rights.
pub trait WorkspaceDirectoryCursorBackend: Send + Sync + 'static {
    /// # Errors
    /// Refuses revoked session/ancestor authority.
    fn check(&self) -> Result<(), String>;
    /// # Errors
    /// Refuses sequence mismatch, native enumeration IO or revoked authority.
    fn page(&self, expected_sequence: u64) -> Result<DirectoryStreamPage, String>;
}

/// Actual held reservation, or guarded forwarding through its existing channel.
/// No public data can construct a native reservation or root verifier.
pub trait WorkspacePendingReplacementBackend: Send + Sync + 'static {
    /// Release only this private controller cache alias after its admitted owner
    /// observed retirement. No File/OFD or creator aliases are bulk-deleted.
    /// # Errors
    /// Refuses unsupported cache retirement or missing admitted ownership.
    fn retire_controller_cache(&self) -> Result<(), String> {
        Err("Owned controller cache retirement unavailable".into())
    }

    /// SAME submitted native job, replaying its actually observed terminal report
    /// once for a newly admitted root client after signed ownership verification.
    /// # Errors
    /// Refuses unavailable terminal receipts or failed signed ownership verification.
    fn acknowledged_committed_job(&self) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        Err("Acknowledged source native terminal receipt unavailable".into())
    }
    /// Returns the actual submitted job's pin only after private signed history
    /// ownership verification. Descriptive receipts cannot construct this cap.
    /// # Errors
    /// Refuses unavailable submitted-job pins or failed signed ownership verification.
    fn acknowledged_committed_pin(
        &self,
    ) -> Result<Arc<dyn WorkspaceReplacementPinBackend>, String> {
        Err("Acknowledged source history ownership unavailable".into())
    }
    /// Retire only the separately owned daemon RPC transport, never the source namespace.
    /// # Errors
    /// Refuses unsupported retirement, an invalid ticket or missing handoff ownership.
    fn retire_daemon_handoff(&self, _ticket: [u8; 16]) -> Result<(), String> {
        Err("source handoff retirement unavailable".into())
    }
    /// # Errors
    /// Refuses unavailable authenticated routes or revoked pending custody.
    fn arm_daemon_handoff(&self, _challenge: [u8; 32]) -> Result<[u8; 16], String> {
        Err("Authenticated source pending route unavailable".into())
    }

    /// # Errors
    /// Revalidates the genuine source and still-held zero-effect work.
    fn check(&self) -> Result<(), String>;
    fn observe(&self) -> Result<PendingReplacementObservation, String>;
    /// # Errors
    /// Consumes once through the native sealed-verifier signed-prepare gate.
    fn start_with_history(&self, receipt: &[u8]) -> Result<Arc<dyn WorkspaceJobBackend>, String>;
    /// # Errors
    /// Releases only this unsubmitted reservation; cannot undo committed effects.
    fn cancel(&self) -> Result<(), String>;
}

/// Actual source-held completed creation custody, not receipt observations.
pub trait WorkspaceCreatedTemporaryBackend: Send + Sync + 'static {
    /// # Errors
    /// Refuses absent creator permissions authority, revoked custody or native IO.
    fn set_permissions(&self, _mode: u32) -> Result<(), String> {
        Err("Actual creator permissions authority unavailable".into())
    }

    /// Source-resident creator custody plus the independently sealed selected
    /// replacement scope, never names or an incoming receipt as authority.
    fn prepare_atomic_save(
        &self,
    ) -> AtomicSavePreparation<Arc<dyn WorkspacePendingReplacementBackend>> {
        AtomicSavePreparation::Refused("Explicit AtomicSave authority unavailable".into())
    }
    /// # Errors
    /// Refuses absent selected replacement custody, revoked scope or native preparation failure.
    fn prepare_replace(&self) -> Result<Arc<dyn WorkspacePendingReplacementBackend>, String> {
        Err("Actual selected replacement custody unavailable".into())
    }

    fn live_scope(&self) -> Option<Result<(), String>> {
        None
    }
    /// # Errors
    /// Refuses changed created inode, source revocation or invalid native receipt.
    fn check(&self) -> Result<(), String>;
    /// # Errors
    /// One-use cleanup retains bytes in protected recovery via a real native job.
    fn retire(&self) -> Result<Arc<dyn WorkspaceJobBackend>, String>;
}
