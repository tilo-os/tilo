//! Typed backend seam shared by native grant owners and guarded RPC relays.
//! These objects never encode credentials, groups, native grants or root claims.
use crate::workspace_server::WorkspaceFileBackend;
use crate::{EntryName, OpenMode};
use std::sync::Arc;
pub use tilo_fs_client::directory_types::*;

pub trait WorkspaceDirectoryBackend: Send + Sync + 'static {
    fn sync_directory(&self) -> Result<(), String> {
        Err("Held source directory sync unavailable".into())
    }

    fn statfs(&self) -> Result<DirectoryFilesystemStats, String> {
        Err("Source filesystem capacity unavailable".into())
    }
    fn check(&self) -> Result<(), String>;
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
    fn recoveries(&self, after: &[u8]) -> Result<RecoveryPage, String> {
        let _ = after;
        Err("Native recovery owner not admitted".into())
    }
    fn restore_captured(
        &self,
        recovery: [u8; 16],
        name: EntryName,
    ) -> Result<Arc<dyn WorkspaceJobBackend>, String> {
        let _ = (recovery, name);
        Err("Native recovery owner not admitted".into())
    }
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
    fn cancel(&self) -> Result<(), String>;
}

/// An independent held-directory cursor, with no byte or mutation rights.
pub trait WorkspaceDirectoryCursorBackend: Send + Sync + 'static {
    fn check(&self) -> Result<(), String>;
    fn page(&self, expected_sequence: u64) -> Result<DirectoryStreamPage, String>;
}
