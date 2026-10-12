//! Exact-inode append custody. Never changes the selected descriptor's flags.
use super::*;
#[cfg(target_os = "linux")]
use std::os::{fd::AsRawFd, unix::fs::MetadataExt as StdMetadataExt};

/// An independent append description of an already-writable selected inode.
/// This is native custody, never a serialized grant or a reopened file name.
pub struct FsAppendHandle {
    session: FsSession,
    file: FileGrant,
    descriptor: std::fs::File,
    identity: NativeIdentity,
}

impl FsSession {
    /// Validate a genuine Write lease and existing positioned writable file.
    /// This probe never reads, creates, truncates, reopens a path or changes
    /// descriptor flags; future writes still recheck live authority.
    /// # Errors
    /// Refuses stale scope, missing Write, readonly/special/append descriptors.
    pub fn validate_write(&self, file: &FileGrant) -> Result<(), FsError> {
        let table = self.call()?;
        let descriptor = self.writable_file(&table, file)?;
        let flags =
            rustix::fs::fcntl_getfl(&*descriptor).map_err(|error| io_error(error.into()))?;
        if flags.contains(OFlags::APPEND) {
            return Err(FsError::Unsupported(
                "append descriptor cannot write at an offset",
            ));
        }
        self.validate(&table, &file.lease, Some(FsRight::Write))
    }

    /// Validate Read+Write rights and an existing positioned read/write descriptor.
    /// No bytes, length, descriptor flags or names are changed.
    /// # Errors
    /// Refuses stale scope, missing rights, non-RDWR or append-only descriptions.
    pub fn validate_read_write(&self, file: &FileGrant) -> Result<(), FsError> {
        let table = self.call()?;
        self.validate(&table, &file.lease, Some(FsRight::Read))?;
        let descriptor = self.writable_file(&table, file)?;
        let flags =
            rustix::fs::fcntl_getfl(&*descriptor).map_err(|error| io_error(error.into()))?;
        if !flags.contains(OFlags::RDWR) {
            return Err(FsError::Denied);
        }
        if flags.contains(OFlags::APPEND) {
            return Err(FsError::Unsupported(
                "append descriptor cannot write at an offset",
            ));
        }
        Ok(())
    }

    /// Open an independent O_APPEND description of exactly the held inode.
    /// # Errors
    /// Refuses absent Write rights, read-only/special descriptors, stale scope,
    /// identity changes, and platforms without exact descriptor reopening.
    pub fn open_append(&self, file: &FileGrant) -> Result<FsAppendHandle, FsError> {
        #[cfg(target_os = "linux")]
        {
            let table = self.call()?;
            let original = self.writable_file(&table, file)?;
            let identity = NativeIdentity::of(&original.metadata().map_err(io_error)?);
            // Only this process's held descriptor magic link is followed. The
            // original Arc stays live across open+fstat, preventing fd reuse.
            // dup/fcntl would share the original OFD and corrupt positioned IO.
            let descriptor = std::fs::OpenOptions::new()
                .append(true)
                .open(format!("/proc/self/fd/{}", original.as_raw_fd()))
                .map_err(io_error)?;
            let metadata = descriptor.metadata().map_err(io_error)?;
            if !metadata.is_file()
                || metadata.dev() != identity.device
                || metadata.ino() != identity.inode
            {
                return Err(FsError::Denied);
            }
            self.validate(&table, &file.lease, Some(FsRight::Write))?;
            Ok(FsAppendHandle {
                session: self.clone(),
                file: file.clone(),
                descriptor,
                identity,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = file;
            Err(FsError::Unsupported(
                "exact-inode append reopening unavailable",
            ))
        }
    }
}

impl FsAppendHandle {
    /// Perform exactly one bounded atomic kernel append; return a partial count.
    /// A lost RPC reply has an unknown effect and must never be retried blindly.
    /// # Errors
    /// Refuses stale or revoked custody and oversized payloads; returns host IO.
    pub fn append(&self, bytes: &[u8]) -> Result<u32, FsError> {
        let table = self.session.call()?;
        let original = self.session.writable_file(&table, &self.file)?;
        if NativeIdentity::of(&original.metadata().map_err(io_error)?) != self.identity {
            return Err(FsError::Denied);
        }
        if bytes.len() > MAX_WRITE_CHUNK as usize {
            return Err(FsError::LimitExceeded);
        }
        if bytes.is_empty() {
            return Ok(0);
        }
        // Attached lifetimes can end independently of the authority mutex.
        // Recheck immediately before the actual effect, like positioned writes.
        self.session
            .validate(&table, &self.file.lease, Some(FsRight::Write))?;
        // Authority remains locked until this single O_APPEND syscall returns.
        let written =
            rustix::io::write(&self.descriptor, bytes).map_err(|error| io_error(error.into()))?;
        u32::try_from(written).map_err(|_| FsError::LimitExceeded)
    }
}
