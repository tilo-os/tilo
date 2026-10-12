//! Explicit child descriptor access through an actual parent grant.
use super::*;

/// Existing-file descriptor access. These modes never create or truncate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeFileAccess {
    Read,
    Write,
    ReadWrite,
}

impl FsSession {
    /// Resolve a regular child with precisely the requested descriptor access.
    /// Rights and lineage come from `parent`, never from names or wire labels.
    /// Existing `lookup` retains its read-only and bounded symlink semantics.
    /// # Errors
    /// Refuses absent parent Enumerate/Read/Write rights, nonregular or symlink
    /// entries, substituted inodes, stale scope, and typed host IO failures.
    pub fn lookup_file_for(
        &self,
        parent: &DirectoryGrant,
        name: &EntryName,
        access: NativeFileAccess,
    ) -> Result<FileGrant, FsError> {
        let mut table = self.call()?;
        let check = |table: &Table| -> Result<(), FsError> {
            self.validate(table, &parent.lease, Some(FsRight::Enumerate))?;
            if matches!(access, NativeFileAccess::Read | NativeFileAccess::ReadWrite) {
                self.validate(table, &parent.lease, Some(FsRight::Read))?;
            }
            if matches!(
                access,
                NativeFileAccess::Write | NativeFileAccess::ReadWrite
            ) {
                self.validate(table, &parent.lease, Some(FsRight::Write))?;
            }
            Ok(())
        };
        check(&table)?;
        let directory = Self::directory(&parent.lease)?;
        let before = directory
            .symlink_metadata(name.as_os_str())
            .map_err(io_error)?;
        if !before.is_file() {
            return Err(FsError::Unsupported("regular child required"));
        }
        let identity = NativeIdentity::of(&before);
        let mode = match access {
            NativeFileAccess::Read => OFlags::RDONLY,
            NativeFileAccess::Write => OFlags::WRONLY,
            NativeFileAccess::ReadWrite => OFlags::RDWR,
        };
        check(&table)?;
        let descriptor = rustix::fs::openat(
            &*directory,
            name.as_os_str(),
            mode | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|e| io_error(e.into()))?;
        let descriptor = File::from_std(std::fs::File::from(descriptor));
        check(&table)?;
        let held = descriptor.metadata().map_err(io_error)?;
        let after = directory
            .symlink_metadata(name.as_os_str())
            .map_err(io_error)?;
        if !held.is_file()
            || !after.is_file()
            || NativeIdentity::of(&held) != identity
            || NativeIdentity::of(&after) != identity
        {
            return Err(FsError::Busy);
        }
        check(&table)?;
        let lease = self.inner.authority.insert(
            &mut table,
            parent.lease.record.owner.clone(),
            parent.lease.record.rights.clone(),
            Some(parent.lease.clone()),
            Some(name.clone()),
            NativeObject::File {
                descriptor: Arc::new(descriptor),
                identity,
            },
        )?;
        Ok(FileGrant { lease })
    }
}
