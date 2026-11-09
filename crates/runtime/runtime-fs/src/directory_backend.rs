//! Typed backend seam shared by native grant owners and guarded RPC relays.
//! These objects never encode credentials, groups, native grants or root claims.
use crate::workspace_server::WorkspaceFileBackend;
use crate::{EntryName, OpenMode};
use std::sync::Arc;
pub use tilo_fs_client::directory_types::*;

pub trait WorkspaceDirectoryBackend: Send + Sync + 'static {
    /// # Errors
    /// Refuses unsupported providers, revoked metadata scope or actual fsync IO.
    fn sync_directory(&self) -> Result<(), String> {
        Err("Held source directory sync unavailable".into())
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
    fn open_cursor(&self) -> Result<Arc<dyn WorkspaceDirectoryCursorBackend>, String>;
    fn metadata(&self) -> Result<DirectoryStat, String>;
    /// # Errors
    /// Observes one child without following links or opening its bytes.
    fn stat(&self, name: &EntryName) -> Result<Option<DirectoryStat>, String>;
    /// Observe raw symlink bytes without granting target traversal or content access.
    fn readlink(&self, name: &EntryName, expected: [u64; 2]) -> Result<Vec<u8>, String>;
    /// Retain a regular inode for metadata only, without byte access.
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
pub trait WorkspaceJobBackend: Send + Sync + 'static {
    /// Release only this private controller cache alias after its admitted owner
    /// observed retirement. No File/OFD or creator aliases are bulk-deleted.
    fn retire_controller_cache(&self) -> Result<(), String> {
        Err("Owned controller cache retirement unavailable".into())
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
