//! Raw-name clients on the capability connection. No native authority is
//! serialized. Append/mutation replies must not be blindly retried after loss.
use crate::directory_types::{DirectoryRows, DirectoryStat, JobPoll, JobUpdate};
use crate::wire::{FsReply, FsRequest};
use crate::{EntryName, FileHandle, OpenMode};
use crate::{connection::Object, native_reply::refused};
use runtime_portal::NotAvailable;
use std::{os::unix::ffi::OsStrExt, sync::Arc};

fn invalid(message: &str) -> NotAvailable {
    NotAvailable::Refused(message.into())
}

fn component(name: &EntryName) -> Result<Vec<u8>, NotAvailable> {
    let bytes = name.as_os_str().as_bytes();
    if bytes.len() > 4096 {
        return Err(invalid("component exceeds bound"));
    }
    Ok(bytes.to_vec())
}

fn identity([device, inode]: [u64; 2]) -> Result<[u64; 2], NotAvailable> {
    if inode == 0 {
        return Err(invalid("invalid native identity"));
    }
    Ok([device, inode])
}

fn native_type(raw: u32, kind: crate::EntryKind) -> Result<u32, NotAvailable> {
    use crate::{EntryKind, directory_types::mode_bits};
    let kinds = [
        (mode_bits(libc::S_IFREG), EntryKind::File),
        (mode_bits(libc::S_IFDIR), EntryKind::Directory),
        (mode_bits(libc::S_IFLNK), EntryKind::Symlink),
        (mode_bits(libc::S_IFIFO), EntryKind::Other),
        (mode_bits(libc::S_IFSOCK), EntryKind::Other),
        (mode_bits(libc::S_IFBLK), EntryKind::Other),
        (mode_bits(libc::S_IFCHR), EntryKind::Other),
    ];
    let expected = kinds
        .into_iter()
        .find_map(|(bits, kind)| (raw == bits).then_some(kind))
        .ok_or_else(|| invalid("invalid native file type"))?;
    if kind != expected {
        return Err(invalid("native file type disagrees with metadata"));
    }
    Ok(raw)
}

// Observations cannot add special modes or any read/write/authority bit.
fn execute_bits(raw: u16) -> Result<u16, NotAvailable> {
    if raw & !0o111 != 0 {
        return Err(invalid("invalid original execute mode bits"));
    }
    Ok(raw)
}

fn observed(stat: DirectoryStat) -> Result<DirectoryStat, NotAvailable> {
    execute_bits(stat.execute_bits)?;
    native_type(stat.file_type, stat.metadata.kind)?;
    identity(stat.identity)?;
    Ok(stat)
}

async fn call(object: &Arc<Object>, request: FsRequest) -> Result<FsReply, NotAvailable> {
    object.call(request).await
}

/// Aliases of the same live directory capability; names carry no authority.
#[derive(Clone)]
pub struct DirectoryClient {
    object: Arc<Object>,
}

impl DirectoryClient {
    /// Alias another capability of the same family on an existing connection.
    pub fn from_object(object: Arc<Object>) -> Self {
        Self { object }
    }

    /// # Errors
    /// Refuses native metadata scope, durability failure or invalid transport.
    pub async fn sync_directory(&self) -> Result<(), NotAvailable> {
        match call(&self.object, FsRequest::SyncDirectory).await? {
            FsReply::Ack(status) => crate::native_reply::check(status),
            _ => Err(crate::unexpected()),
        }
    }

    /// # Errors
    /// Refuses source scope/transport loss and invalid native capacity metadata.
    pub async fn statfs(
        &self,
    ) -> Result<crate::directory_types::DirectoryFilesystemStats, NotAvailable> {
        match call(&self.object, FsRequest::Statfs).await? {
            FsReply::FilesystemStats(stats) => {
                let stats = stats.ok_or_else(refused)?;
                stats.validate().map_err(|error| invalid(&error))?;
                Ok(stats)
            }
            _ => Err(crate::unexpected()),
        }
    }

    /// # Errors
    /// Refuses enumeration admission or transport/authority failure.
    pub async fn open_cursor(&self) -> Result<DirectoryCursorClient, NotAvailable> {
        match call(&self.object, FsRequest::OpenCursor).await? {
            FsReply::Handle(handle) => Ok(DirectoryCursorClient {
                object: crate::handle_reply(handle, &self.object)?,
            }),
            _ => Err(crate::unexpected()),
        }
    }

    /// # Errors
    /// Refuses absent entry, invalid target or transport loss.
    pub async fn readlink(
        &self,
        name: &EntryName,
        expected: [u64; 2],
    ) -> Result<Vec<u8>, NotAvailable> {
        let target = match call(
            &self.object,
            FsRequest::Readlink {
                name: component(name)?,
                expected,
            },
        )
        .await?
        {
            FsReply::Bytes(target) => target.ok_or_else(refused)?,
            _ => return Err(crate::unexpected()),
        };
        if target.is_empty() || target.len() > 4096 || target.contains(&0) {
            return Err(invalid("invalid native symlink target"));
        }
        Ok(target)
    }

    /// # Errors
    /// Refuses revoked scope or transport failure.
    pub async fn metadata(&self) -> Result<DirectoryStat, NotAvailable> {
        match call(&self.object, FsRequest::DirectoryMetadata).await? {
            FsReply::Stat(stat) => observed(stat.ok_or_else(refused)?),
            _ => Err(crate::unexpected()),
        }
    }

    /// # Errors
    /// Refuses native enumeration failure or transport loss.
    pub async fn stat(&self, name: &EntryName) -> Result<Option<DirectoryStat>, NotAvailable> {
        match call(
            &self.object,
            FsRequest::StatEntry {
                name: component(name)?,
            },
        )
        .await?
        {
            FsReply::EntryStat(stat) => stat.ok_or_else(refused)?.map(observed).transpose(),
            _ => Err(crate::unexpected()),
        }
    }

    /// Retain metadata custody without any byte IO capability.
    pub async fn pin_metadata(
        &self,
        name: &EntryName,
        expected: [u64; 2],
    ) -> Result<MetadataPinClient, NotAvailable> {
        let FsReply::Handle(handle) = call(
            &self.object,
            FsRequest::PinEntry {
                name: component(name)?,
                expected,
            },
        )
        .await?
        else {
            return Err(crate::unexpected());
        };
        let pin = MetadataPinClient {
            object: crate::handle_reply(handle, &self.object)?,
        };
        let actual = pin.metadata().await?;
        if actual.identity != expected || actual.metadata.kind != crate::EntryKind::File {
            return Err(invalid("metadata pin identity/type differs"));
        }
        Ok(pin)
    }

    /// # Errors
    /// Refuses stale/foreign continuations, oversized pages, revocation or transport loss.
    pub async fn list(&self, cursor: u64) -> Result<DirectoryRows, NotAvailable> {
        let rows = match call(&self.object, FsRequest::ListRows { cursor }).await? {
            FsReply::Rows(rows) => rows.ok_or_else(refused)?,
            _ => return Err(crate::unexpected()),
        };
        if rows.entries.len() > 256 {
            return Err(invalid("directory page exceeds bound"));
        }
        Ok(rows)
    }

    /// # Errors
    /// Refuses non-directory, invalid-name, missing-right and revoked scopes.
    pub async fn directory(&self, name: &EntryName) -> Result<Self, NotAvailable> {
        match call(
            &self.object,
            FsRequest::OpenDirectory {
                name: component(name)?,
            },
        )
        .await?
        {
            FsReply::Handle(handle) => Ok(Self {
                object: crate::handle_reply(handle, &self.object)?,
            }),
            _ => Err(crate::unexpected()),
        }
    }

    /// Open an existing no-follow child. Missing files require an actual create job.
    pub async fn open_file(
        &self,
        name: &EntryName,
        mode: OpenMode,
    ) -> Result<FileHandle, NotAvailable> {
        let observed = self
            .stat(name)
            .await?
            .ok_or_else(|| invalid("native file missing"))?;
        self.open_file_expected(name, mode, observed.identity).await
    }

    /// # Errors
    /// Refuses a changed named inode before an actual open/truncate effect.
    pub async fn open_file_expected(
        &self,
        name: &EntryName,
        mode: OpenMode,
        expected: [u64; 2],
    ) -> Result<FileHandle, NotAvailable> {
        identity(expected)?;
        let opened = match call(
            &self.object,
            FsRequest::OpenFile {
                name: component(name)?,
                mode,
                expected,
            },
        )
        .await?
        {
            FsReply::OpenedFile(opened) => opened.ok_or_else(refused)?,
            _ => return Err(crate::unexpected()),
        };
        if identity([opened.device, opened.inode])? != expected {
            return Err(invalid("opened native identity differs"));
        }
        Ok(FileHandle {
            object: crate::handle_reply(Some(opened.handle), &self.object)?,
        })
    }

    /// # Errors
    /// Refuses missing owner store, queue admission, revoked scope or transport loss.
    pub async fn create_file(&self, name: &EntryName) -> Result<NativeJobClient, NotAvailable> {
        self.job(FsRequest::CreateFile {
            name: component(name)?,
        })
        .await
    }

    /// # Errors
    /// Refuses missing owner store, queue admission, revoked scope or transport loss.
    pub async fn create_directory(
        &self,
        name: &EntryName,
    ) -> Result<NativeJobClient, NotAvailable> {
        self.job(FsRequest::CreateDirectory {
            name: component(name)?,
        })
        .await
    }

    async fn job(&self, request: FsRequest) -> Result<NativeJobClient, NotAvailable> {
        match call(&self.object, request).await? {
            FsReply::Handle(handle) => Ok(NativeJobClient {
                object: crate::handle_reply(handle, &self.object)?,
            }),
            _ => Err(crate::unexpected()),
        }
    }

    /// # Errors
    /// Requires a genuine current scoped directory capability; no IDs grant access.
    pub async fn recoveries(
        &self,
        after: &[u8],
    ) -> Result<crate::directory_types::RecoveryPage, NotAvailable> {
        if !after.is_empty() && after.len() != 24 {
            return Err(invalid("Invalid recovery continuation"));
        }
        let page = match call(
            &self.object,
            FsRequest::Recoveries {
                after: after.to_vec(),
            },
        )
        .await?
        {
            FsReply::Recoveries(page) => page.ok_or_else(refused)?,
            _ => return Err(crate::unexpected()),
        };
        let next = &page.next;
        if page.entries.len() > 256
            || (!next.is_empty() && next.len() != 24)
            || page.done != next.is_empty()
            || (!page.done && (page.entries.is_empty() || next.as_slice() == after))
        {
            return Err(invalid("Invalid recovery page bounds"));
        }
        Ok(page)
    }

    /// # Errors
    /// Never retries a lost mutation reply. Parent and source scope remain genuine.
    pub async fn restore_captured(
        &self,
        recovery: [u8; 16],
        name: &EntryName,
    ) -> Result<NativeJobClient, NotAvailable> {
        if recovery == [0; 16] {
            return Err(invalid("Invalid recovery ID"));
        }
        self.job(FsRequest::RestoreCaptured {
            recovery,
            name: component(name)?,
        })
        .await
    }

    /// # Errors
    /// Refuses native lookup/admission, revoked scope or transport loss.
    pub async fn rename_no_replace(
        &self,
        name: &EntryName,
        new_name: &EntryName,
    ) -> Result<NativeJobClient, NotAvailable> {
        self.job(FsRequest::RenameNoReplace {
            name: component(name)?,
            new_name: component(new_name)?,
        })
        .await
    }
}

/// Single custody of draining native job updates. IDs identify actual receipts;
/// fetching a report still requires the actual scoped `OperationOwner`.
pub struct NativeJobClient {
    object: Arc<Object>,
}

impl NativeJobClient {
    /// Alias another capability of the same family on an existing connection.
    pub fn from_object(object: Arc<Object>) -> Self {
        Self { object }
    }

    /// # Errors
    /// Refuses unavailable, revoked or disconnected native job scope.
    pub async fn check_scope(&self) -> Result<(), NotAvailable> {
        self.empty(FsRequest::JobCheckScope).await
    }

    async fn empty(&self, request: FsRequest) -> Result<(), NotAvailable> {
        match call(&self.object, request).await? {
            FsReply::Empty => Ok(()),
            _ => Err(crate::unexpected()),
        }
    }

    /// # Errors
    /// Refuses a revoked job or an oversized report.
    pub async fn poll(&mut self) -> Result<JobPoll, NotAvailable> {
        let poll = match call(&self.object, FsRequest::JobPoll).await? {
            FsReply::Poll(poll) => poll.ok_or_else(refused)?,
            _ => return Err(crate::unexpected()),
        };
        if poll.updates.len() > 1025 {
            return Err(invalid("job updates exceed bound"));
        }
        for update in &poll.updates {
            match update {
                JobUpdate::Progress { phase, .. } if phase.len() > 4096 => {
                    return Err(invalid("phase exceeds bound"));
                }
                JobUpdate::Item {
                    error, recoveries, ..
                } if error.as_ref().is_some_and(|error| error.len() > 4096)
                    || recoveries.len() > 256 =>
                {
                    return Err(invalid("item exceeds bound"));
                }
                _ => {}
            }
        }
        Ok(poll)
    }

    /// Ask for canonical job cancellation. An in-flight native commit may already
    /// have happened; callers must consume its actual terminal outcome.
    pub async fn cancel(&self) -> Result<(), NotAvailable> {
        match call(&self.object, FsRequest::JobCancel).await? {
            FsReply::Ack(status) => crate::native_reply::check(status),
            _ => Err(crate::unexpected()),
        }
    }
}

/// Metadata-only held regular inode; this type exposes no read/write methods.
#[derive(Clone)]
pub struct MetadataPinClient {
    object: Arc<Object>,
}

impl MetadataPinClient {
    /// Alias another capability of the same family on an existing connection.
    pub fn from_object(object: Arc<Object>) -> Self {
        Self { object }
    }

    /// # Errors
    /// Refuses unavailable, revoked or disconnected metadata custody.
    pub async fn check_scope(&self) -> Result<(), NotAvailable> {
        match call(&self.object, FsRequest::PinCheckScope).await? {
            FsReply::Empty => Ok(()),
            _ => Err(crate::unexpected()),
        }
    }

    /// # Errors
    /// Refuses revoked scope, invalid observation or transport failure.
    pub async fn metadata(&self) -> Result<DirectoryStat, NotAvailable> {
        match call(&self.object, FsRequest::PinStat).await? {
            FsReply::Stat(stat) => {
                let stat = observed(stat.ok_or_else(refused)?)?;
                if stat.metadata.kind != crate::EntryKind::File {
                    return Err(invalid("metadata pin must hold regular inode"));
                }
                Ok(stat)
            }
            _ => Err(crate::unexpected()),
        }
    }
}

/// One live enumeration cursor; pages are bounded and sequence-checked.
#[derive(Clone)]
pub struct DirectoryCursorClient {
    object: Arc<Object>,
}

impl DirectoryCursorClient {
    /// Alias another capability of the same family on an existing connection.
    pub fn from_object(object: Arc<Object>) -> Self {
        Self { object }
    }

    /// # Errors
    /// Refuses sequence mismatch, native enumeration or transport/authority loss.
    pub async fn page(
        &self,
        expected: u64,
    ) -> Result<crate::directory_types::DirectoryStreamPage, NotAvailable> {
        let page = match call(
            &self.object,
            FsRequest::CursorPage {
                expected_sequence: expected,
            },
        )
        .await?
        {
            FsReply::StreamPage(page) => page.ok_or_else(refused)?,
            _ => return Err(crate::unexpected()),
        };
        if page.next_sequence
            != expected
                .checked_add(1)
                .ok_or_else(|| invalid("cursor sequence exhausted"))?
        {
            return Err(invalid("invalid cursor sequence"));
        }
        if page.entries.len() > 128 {
            return Err(invalid("cursor page exceeds bound"));
        }
        for row in &page.entries {
            if let Some(stat) = &row.stat {
                observed(stat.clone())?;
            }
        }
        Ok(page)
    }
}

#[cfg(test)]
mod native_mode_tests {
    use super::native_type;
    use crate::{EntryKind, directory_types::mode_bits};

    #[test]
    fn mode_width_and_metadata_kind_preserve_wire_validation() {
        assert_eq!(mode_bits(0o170_000_u16), mode_bits(0o170_000_u32));
        for (bits, kind) in [
            (libc::S_IFREG, EntryKind::File),
            (libc::S_IFDIR, EntryKind::Directory),
            (libc::S_IFLNK, EntryKind::Symlink),
            (libc::S_IFIFO, EntryKind::Other),
            (libc::S_IFSOCK, EntryKind::Other),
            (libc::S_IFBLK, EntryKind::Other),
            (libc::S_IFCHR, EntryKind::Other),
        ] {
            let raw = mode_bits(bits);
            assert_eq!(native_type(raw, kind).unwrap(), raw);
            let wrong_kind = if kind == EntryKind::File {
                EntryKind::Directory
            } else {
                EntryKind::File
            };
            assert!(native_type(raw, wrong_kind).is_err());
        }
        assert!(native_type(0, EntryKind::Other).is_err());
        assert!(native_type(u32::MAX, EntryKind::Other).is_err());
    }
}

#[cfg(test)]
mod filesystem_stats_tests {
    use crate::directory_types::DirectoryFilesystemStats;

    fn stats() -> DirectoryFilesystemStats {
        DirectoryFilesystemStats {
            blocks: 100,
            blocks_free: 50,
            blocks_available: 40,
            files: 80,
            files_free: 30,
            files_available: 20,
            block_size: 4096,
            fragment_size: 4096,
            name_max: 255,
            read_only: false,
        }
    }

    #[test]
    fn invalid_capacity_is_refused() {
        assert!(stats().validate().is_ok());
        for change in 0..8 {
            let mut stats = stats();
            match change {
                0 => stats.blocks_free = 101,
                1 => stats.blocks_available = 51,
                2 => stats.files_free = 81,
                3 => stats.files_available = 31,
                4 => stats.block_size = 0,
                5 => stats.fragment_size = 0,
                6 => stats.name_max = 0,
                _ => stats.blocks = u64::MAX,
            }
            assert!(stats.validate().is_err());
        }
    }
}

#[cfg(test)]
mod execute_mode_codec_tests {
    use super::execute_bits;

    #[test]
    fn execute_bits_reject_read_write_and_special_modes() {
        for value in [0, 1, 0o10, 0o100, 0o111] {
            assert_eq!(execute_bits(value).unwrap(), value);
        }
        for value in [0o2, 0o200, 0o400, 0o777, 0o1000, 0o4000, u16::MAX] {
            assert!(execute_bits(value).is_err());
        }
    }
}

#[cfg(test)]
mod absent_stat_tests {
    use super::observed;
    use crate::directory_types::mode_bits;
    use crate::{EntryKind, Metadata};

    fn stat(kind: EntryKind, file_type: u32, execute: u16, inode: u64) -> super::DirectoryStat {
        super::DirectoryStat {
            metadata: Metadata {
                kind,
                size: 3,
                modified_unix_seconds: None,
                needs_hydration: false,
            },
            identity: [2, inode],
            file_type,
            execute_bits: execute,
        }
    }

    #[test]
    fn an_absent_claim_cannot_carry_an_observation() {
        let valid = || stat(EntryKind::File, mode_bits(libc::S_IFREG), 0o111, 4);
        assert!(observed(valid()).is_ok());
        for claim in 0..4 {
            let mut stat = valid();
            match claim {
                0 => stat.execute_bits = 0o777,
                1 => stat.identity = [3, 0],
                2 => stat.file_type = mode_bits(libc::S_IFDIR),
                _ => stat.file_type = 0,
            }
            assert!(observed(stat).is_err(), "accepted absent claim {claim}");
        }
    }
}
