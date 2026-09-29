//! Existing-only, resource-only private sender storage. No owner or send authority.
pub use crate::portable_storage::PrivateActionsCapture;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SenderStorageCode {
    Capacity,
    StaleCapture,
    InvalidScope,
    InvalidFile,
    Busy,
    NotPublished,
    Uncertain,
    Io,
}
#[cfg(test)]
mod tests;

pub(crate) fn copy_bytes(bytes: &[u8]) -> Result<Vec<u8>, SenderStorageCode> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(bytes.len())
        .map_err(|_| SenderStorageCode::Capacity)?;
    result.extend_from_slice(bytes);
    Ok(result)
}
pub(crate) fn copy_string(value: &str) -> Result<String, SenderStorageCode> {
    let mut result = String::new();
    result
        .try_reserve_exact(value.len())
        .map_err(|_| SenderStorageCode::Capacity)?;
    result.push_str(value);
    Ok(result)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod administration;
mod codec;
/// Inert fixed-layout sender identity. No constructor establishes an owner.
#[derive(PartialEq, Eq)]
pub struct SenderScope {
    profile: u8,
    room: [u8; 16],
    root: [u8; 32],
    device: [u8; 32],
    machine: [u8; 256],
    machine_len: u16,
    incarnation: [u8; 16],
}
impl SenderScope {
    pub fn native(
        room: uuid::Uuid,
        root: [u8; 32],
        device: [u8; 32],
    ) -> Result<Self, SenderStorageCode> {
        if room.is_nil() || root == [0; 32] || device == [0; 32] {
            return Err(SenderStorageCode::InvalidScope);
        }
        Ok(Self {
            profile: 1,
            room: *room.as_bytes(),
            root,
            device,
            machine: [0; 256],
            machine_len: 0,
            incarnation: [0; 16],
        })
    }
    pub fn cloud(
        room: uuid::Uuid,
        root: [u8; 32],
        device: [u8; 32],
        machine: &str,
        incarnation: [u8; 16],
    ) -> Result<Self, SenderStorageCode> {
        if machine.is_empty()
            || machine.len() > 256
            || machine.chars().any(char::is_control)
            || incarnation == [0; 16]
        {
            return Err(SenderStorageCode::InvalidScope);
        }
        let mut scope = Self::native(room, root, device)?;
        scope.profile = 2;
        scope.machine_len = machine.len() as u16;
        scope.machine[..machine.len()].copy_from_slice(machine.as_bytes());
        scope.incarnation = incarnation;
        Ok(scope)
    }
}

use crate::portable_storage::PortableStorage;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::ffi::OsStr;
use std::io::{self, Read};
#[cfg(any(target_os = "macos", target_os = "linux"))]
use tilo_files::native::atomic::bounded::{
    AdmissionError, BoundedOwnerDirectory as OwnerDirectory, ChildOpenError,
};
use tilo_files::native::atomic::lease::FileLease;
use tilo_files::native::atomic::{
    ExistingCommitError, PublicationError, PublicationState, StorageDirectory,
};
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
type OwnerDirectory = StorageDirectory;
#[cfg(any(target_os = "macos", target_os = "linux"))]
const FILE_NAME: &str = "private-actions-v1.bin";
#[cfg(any(target_os = "macos", target_os = "linux"))]
const CHILD_NAME: &str = "private-actions-v1";
#[cfg(any(target_os = "macos", target_os = "linux"))]
const LOCK_NAME: &str = "private-actions-v1.writer.lock";
#[derive(Clone, Copy, PartialEq, Eq)]
struct Cursor {
    instance: [u8; 16],
    sequence: u64,
    digest: [u8; 32],
}
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    expect(
        dead_code,
        reason = "only macOS and Linux have the bounded private store; elsewhere every open refuses"
    )
)]
pub struct PrivateActionsFile {
    root: OwnerDirectory,
    directory: OwnerDirectory,
    root_identity: tilo_files::native::atomic::FileIdentity,
    child_identity: tilo_files::native::atomic::FileIdentity,
    lease: FileLease,
    cache: PortableStorage,
    workspace: [u8; 36],
    scope: SenderScope,
    selected_pending_revision: Option<u64>,
    cursor: Option<Cursor>,
    cursor_identity: Option<tilo_files::native::atomic::FileIdentity>,
}
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    expect(
        dead_code,
        reason = "only macOS and Linux have the bounded private store; elsewhere every open refuses"
    )
)]
pub struct PreparedPrivateWrite {
    capture: PrivateActionsCapture,
    encoded: Vec<u8>,
    file: PrivateActionsFile,
    proposed: Cursor,
}
pub struct CommittedPrivateWrite {
    prepared: PreparedPrivateWrite,
}
pub struct FailedPrivateWrite {
    error: PublicationError,
    prepared: PreparedPrivateWrite,
}
pub struct PrivateWriteRefusal {
    error: Option<OpenError>,
    prepared: PreparedPrivateWrite,
    code: SenderStorageCode,
}
pub struct CommittedPrivateRead {
    input: std::fs::File,
    bytes: Vec<u8>,
    file: PrivateActionsFile,
}
pub struct FailedPrivateRead {
    input: Option<std::fs::File>,
    candidate: Option<Cursor>,
    existing: Option<ExistingCommitError>,
    error: Option<OpenError>,
    bytes: Vec<u8>,
    file: PrivateActionsFile,
    code: SenderStorageCode,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct PrivateCommitStamp {
    cursor: Cursor,
    envelope_revision: u64,
    cache_revision: u64,
    payload_length: usize,
    payload_digest: [u8; 32],
}
pub struct CurrentPrivateState {
    file: PrivateActionsFile,
    revision: u64,
    stamp: PrivateCommitStamp,
}
pub struct CurrentPrivateBorrow<'a> {
    view: crate::portable_storage::PrivateCurrentBorrow<'a>,
    instance: [u8; 16],
    sequence: u64,
}
pub enum PrivateQueueOutcome {
    Unchanged(CurrentPrivateState),
    Pending(PrivateActionsFile),
}
pub struct QueueRefusal {
    state: CurrentPrivateState,
    payload: Vec<u8>,
    code: SenderStorageCode,
}
pub struct HydrateRefusal {
    error: Option<OpenError>,
    read: CommittedPrivateRead,
    code: SenderStorageCode,
}

pub struct PrepareRefusal {
    file: PrivateActionsFile,
    capture: PrivateActionsCapture,
    code: SenderStorageCode,
}
pub struct FinishRefusal {
    error: Option<OpenError>,
    committed: CommittedPrivateWrite,
    code: SenderStorageCode,
}
pub enum PrivateWriteOutcome {
    Committed(CommittedPrivateWrite),
    Failed(FailedPrivateWrite),
    Refused(PrivateWriteRefusal),
}
pub enum PrivateReadOutcome {
    Committed(CommittedPrivateRead),
    Failed(FailedPrivateRead),
}
// Failure owns every admitted object; none can be extracted as a healthy file.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux")),
    expect(
        dead_code,
        reason = "only macOS and Linux have the bounded private store; elsewhere every open refuses"
    )
)]
pub struct FileOpenRefusal {
    error: Option<OpenError>,
    original: Option<StorageDirectory>,
    root: Option<OwnerDirectory>,
    child: Option<OwnerDirectory>,
    lease: Option<FileLease>,
    cache: PortableStorage,
    scope: SenderScope,
    code: SenderStorageCode,
}
enum OpenError {
    Io(io::Error),
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Admission(AdmissionError),
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Child(ChildOpenError),
}
impl From<io::Error> for OpenError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<io::ErrorKind> for OpenError {
    fn from(value: io::ErrorKind) -> Self {
        Self::Io(value.into())
    }
}
impl OpenError {
    fn io_error(&self) -> &io::Error {
        match self {
            Self::Io(e) => e,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Admission(e) => e.io_error(),
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Child(e) => e.io_error(),
        }
    }
}
impl FileOpenRefusal {
    pub fn code(&self) -> SenderStorageCode {
        self.code
    }
    pub fn io_error(&self) -> Option<&io::Error> {
        self.error.as_ref().map(OpenError::io_error)
    }
}
impl PrivateActionsFile {
    pub fn from_held(
        directory: StorageDirectory,
        scope: SenderScope,
        cache: &PortableStorage,
    ) -> Result<Self, FileOpenRefusal> {
        #[cfg_attr(
            not(any(target_os = "macos", target_os = "linux")),
            expect(
                unused_mut,
                reason = "only macOS and Linux have the bounded private store; elsewhere every open refuses"
            )
        )]
        let mut held = FileOpenRefusal {
            error: None,
            original: Some(directory),
            root: None,
            child: None,
            lease: None,
            cache: cache.clone(),
            scope,
            code: SenderStorageCode::Capacity,
        };
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let admitted = match held.original.take().map(OwnerDirectory::admit) {
                Some(Ok(root)) => root,
                Some(Err(e)) => {
                    held.error = Some(OpenError::Admission(e));
                    held.code = SenderStorageCode::Io;
                    return Err(held);
                }
                None => {
                    held.code = SenderStorageCode::Io;
                    return Err(held);
                }
            };
            let root = held.root.insert(admitted);
            let opened = (|| -> Result<_, OpenError> {
                let sentinel = root
                    .open_lease_existing(OsStr::new(LOCK_NAME))
                    .map_err(OpenError::Io)?;
                let lease = match FileLease::try_acquire(
                    sentinel,
                    tilo_files::native::atomic::lease::LeaseMode::Exclusive,
                ) {
                    Ok(lease) => held.lease.insert(lease),
                    Err(tilo_files::native::atomic::lease::AcquireError::Busy) => return Ok(None),
                    Err(tilo_files::native::atomic::lease::AcquireError::Io(e)) => {
                        return Err(OpenError::Io(e));
                    }
                };
                let child = root
                    .open_directory_child(OsStr::new(CHILD_NAME))
                    .map_err(OpenError::Child)?;
                let child = held.child.insert(child);
                let root_identity = root.identity().map_err(OpenError::Io)?;
                let child_identity = child.identity().map_err(OpenError::Io)?;
                let named = root
                    .open_lease_existing(OsStr::new(LOCK_NAME))
                    .map_err(OpenError::Io)?;
                if tilo_files::native::atomic::FileIdentity::of(&named).map_err(OpenError::Io)?
                    != lease.identity().map_err(OpenError::Io)?
                {
                    return Err(OpenError::Io(io::ErrorKind::InvalidData.into()));
                }
                Ok(Some((root_identity, child_identity)))
            })();
            let (root_identity, child_identity) = match opened {
                Ok(Some(v)) => v,
                Ok(None) => {
                    held.code = SenderStorageCode::Busy;
                    return Err(held);
                }
                Err(e) => {
                    held.error = Some(e);
                    held.code = SenderStorageCode::Io;
                    return Err(held);
                }
            };
            let mut workspace = [0; 36];
            uuid::Uuid::from_bytes(held.scope.room)
                .hyphenated()
                .encode_lower(&mut workspace);
            let (Some(root), Some(directory), Some(lease)) =
                (held.root.take(), held.child.take(), held.lease.take())
            else {
                held.code = SenderStorageCode::Io;
                return Err(held);
            };
            Ok(Self {
                root,
                directory,
                root_identity,
                child_identity,
                lease,
                cache: held.cache,
                workspace,
                scope: held.scope,
                selected_pending_revision: None,
                cursor: None,
                cursor_identity: None,
            })
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(held)
        }
    }
    pub fn cache(&self) -> &PortableStorage {
        &self.cache
    }
    /// The room's canonical hyphenated UUID.
    ///
    /// # Panics
    ///
    /// Never: the workspace is always a lowercase ASCII UUID encoding.
    #[expect(
        clippy::expect_used,
        reason = "the workspace buffer is filled by Uuid::encode_lower and is always ASCII"
    )]
    pub fn workspace(&self) -> &str {
        std::str::from_utf8(&self.workspace).expect("canonical UUID")
    }
    fn bindings(&self) -> Result<(), OpenError> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.root.validate()?;
            self.directory.validate()?;
            if self.root.identity()? != self.root_identity
                || self.directory.identity()? != self.child_identity
            {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let child = self
                .root
                .open_directory_child(OsStr::new(CHILD_NAME))
                .map_err(OpenError::Child)?;
            let named = self.root.open_lease_existing(OsStr::new(LOCK_NAME))?;
            if child.identity()? != self.child_identity
                || tilo_files::native::atomic::FileIdentity::of(&named)? != self.lease.identity()?
            {
                return Err(io::ErrorKind::InvalidData.into());
            }
            Ok(())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
    fn open_input(&self) -> Result<std::fs::File, OpenError> {
        self.bindings()?;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            self.directory
                .open_read(OsStr::new(FILE_NAME))
                .map_err(OpenError::Io)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
    fn check_cursor(&self) -> Result<tilo_files::native::atomic::FileIdentity, OpenError> {
        let cursor = self
            .cursor
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        let mut input = self.open_input()?;
        let identity = tilo_files::native::atomic::FileIdentity::of(&input)?;
        if self
            .cursor_identity
            .as_ref()
            .is_some_and(|v| v != &identity)
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut hash = blake3::Hasher::new();
        let mut scratch = [0u8; 8192];
        let mut total = 0usize;
        loop {
            let n = match input.read(&mut scratch) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if n == 0 {
                break;
            }
            total = total
                .checked_add(n)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
            if total > codec::MAX_FILE {
                return Err(io::ErrorKind::InvalidData.into());
            }
            hash.update(&scratch[..n]);
        }
        if *hash.finalize().as_bytes() != cursor.digest {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.bindings()?;
        Ok(identity)
    }
    pub fn read_existing(self) -> PrivateReadOutcome {
        let mut phase = FailedPrivateRead {
            input: None,
            candidate: None,
            existing: None,
            error: None,
            bytes: Vec::new(),
            file: self,
            code: SenderStorageCode::Capacity,
        };
        let result = (|| -> Result<(), SenderStorageCode> {
            phase
                .bytes
                .try_reserve_exact(codec::MAX_FILE + 1)
                .map_err(|_| SenderStorageCode::Capacity)?;
            phase.bytes.resize(codec::MAX_FILE + 1, 0);
            let mut input = phase.file.open_input().map_err(|e| {
                phase.error = Some(e);
                SenderStorageCode::Io
            })?;
            let mut length = 0;
            while length < phase.bytes.len() {
                match input.read(&mut phase.bytes[length..]) {
                    Ok(0) => break,
                    Ok(n) => length += n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        phase.error = Some(OpenError::Io(e));
                        return Err(SenderStorageCode::Io);
                    }
                }
            }
            phase.bytes.truncate(length);
            let parsed = codec::decode(&phase.file.scope, &phase.bytes)?;
            phase.candidate = Some(Cursor {
                instance: parsed.instance,
                sequence: parsed.sequence,
                digest: *blake3::hash(&phase.bytes).as_bytes(),
            });
            let identity = tilo_files::native::atomic::FileIdentity::of(&input).map_err(|e| {
                phase.error = Some(OpenError::Io(e));
                SenderStorageCode::Io
            })?;
            phase.input = Some(input);
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            phase
                .file
                .directory
                .commit_existing(OsStr::new(FILE_NAME), &identity)
                .map_err(|e| {
                    phase.existing = Some(e);
                    SenderStorageCode::Uncertain
                })?;
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                let _ = identity;
                Err(SenderStorageCode::Capacity)
            }
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Ok(())
        })();
        if let Err(code) = result {
            phase.code = code;
            return PrivateReadOutcome::Failed(phase);
        }
        phase.complete()
    }
    pub fn capture(&self) -> Result<PrivateActionsCapture, SenderStorageCode> {
        PrivateActionsCapture::capture_for_revision(
            &self.cache,
            self.workspace(),
            self.selected_pending_revision
                .ok_or(SenderStorageCode::StaleCapture)?,
        )
    }
    fn stamp(
        &self,
        payload: &[u8],
        envelope_revision: u64,
        cache_revision: u64,
    ) -> Option<PrivateCommitStamp> {
        Some(PrivateCommitStamp {
            cursor: self.cursor?,
            envelope_revision,
            cache_revision,
            payload_length: payload.len(),
            payload_digest: *blake3::hash(payload).as_bytes(),
        })
    }
    pub fn prepare(
        self,
        capture: PrivateActionsCapture,
    ) -> Result<PreparedPrivateWrite, PrepareRefusal> {
        let result = (|| {
            if !capture.bound_to(&self.cache, self.workspace()) {
                return Err(SenderStorageCode::InvalidScope);
            }
            if self.selected_pending_revision != Some(capture.revision()) {
                return Err(SenderStorageCode::StaleCapture);
            }
            capture.check_current()?;
            let cursor = self.cursor.ok_or(SenderStorageCode::InvalidFile)?;
            if self.cursor_identity.is_none() {
                return Err(SenderStorageCode::InvalidFile);
            }
            let sequence = cursor
                .sequence
                .checked_add(1)
                .ok_or(SenderStorageCode::Capacity)?;
            let encoded = codec::encode(
                &self.scope,
                cursor.instance,
                sequence,
                capture.revision(),
                capture.bytes(),
            )?;
            let proposed = Cursor {
                instance: cursor.instance,
                sequence,
                digest: *blake3::hash(&encoded).as_bytes(),
            };
            Ok((encoded, proposed))
        })();
        match result {
            Ok((encoded, proposed)) => Ok(PreparedPrivateWrite {
                capture,
                encoded,
                file: self,
                proposed,
            }),
            Err(code) => Err(PrepareRefusal {
                file: self,
                capture,
                code,
            }),
        }
    }
}
impl PreparedPrivateWrite {
    fn committed(mut self) -> PrivateWriteOutcome {
        self.file.cursor = Some(self.proposed);
        // The post-barrier fence establishes the newly published named inode before finish.
        self.file.cursor_identity = None;
        PrivateWriteOutcome::Committed(CommittedPrivateWrite { prepared: self })
    }
    pub fn publish(self) -> PrivateWriteOutcome {
        if let Err(code) = self.capture.check_current() {
            return PrivateWriteOutcome::Refused(PrivateWriteRefusal {
                error: None,
                prepared: self,
                code,
            });
        }
        if let Err(error) = self.file.check_cursor() {
            return PrivateWriteOutcome::Refused(PrivateWriteRefusal {
                error: Some(error),
                prepared: self,
                code: SenderStorageCode::Io,
            });
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            match self.file.directory.publish(
                OsStr::new(FILE_NAME),
                &self.encoded,
                tilo_files::native::atomic::PublishMode::Replace,
            ) {
                Ok(()) => self.committed(),
                Err(error) => PrivateWriteOutcome::Failed(FailedPrivateWrite {
                    error,
                    prepared: self,
                }),
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            PrivateWriteOutcome::Refused(PrivateWriteRefusal {
                error: None,
                prepared: self,
                code: SenderStorageCode::Capacity,
            })
        }
    }
}
impl PrivateWriteRefusal {
    pub fn code(&self) -> SenderStorageCode {
        self.code
    }
    pub fn io_error(&self) -> Option<&io::Error> {
        self.error.as_ref().map(OpenError::io_error)
    }
    pub fn into_prepared(self) -> PreparedPrivateWrite {
        self.prepared
    }
}
impl FailedPrivateWrite {
    pub fn publication_error(&self) -> &PublicationError {
        &self.error
    }
    pub fn state(&self) -> PublicationState {
        self.error.state()
    }
    pub fn retry_commit(self) -> PrivateWriteOutcome {
        match self.error.retry_commit() {
            Ok(()) => self.prepared.committed(),
            Err(error) => PrivateWriteOutcome::Failed(Self {
                error,
                prepared: self.prepared,
            }),
        }
    }
}
impl CommittedPrivateWrite {
    pub fn finish_current(mut self) -> Result<CurrentPrivateState, FinishRefusal> {
        let identity = match self.prepared.file.check_cursor() {
            Ok(identity) => identity,
            Err(error) => {
                return Err(FinishRefusal {
                    error: Some(error),
                    committed: self,
                    code: SenderStorageCode::Io,
                });
            }
        };
        self.prepared.file.cursor_identity = Some(identity);
        let revision = self.prepared.capture.revision();
        let Some(stamp) =
            self.prepared
                .file
                .stamp(self.prepared.capture.bytes(), revision, revision)
        else {
            return Err(FinishRefusal {
                error: None,
                committed: self,
                code: SenderStorageCode::Io,
            });
        };
        match self.prepared.capture.finish_physical(Some(stamp)) {
            Ok(revision) => Ok(CurrentPrivateState {
                file: self.prepared.file,
                revision,
                stamp,
            }),
            Err(code) => Err(FinishRefusal {
                error: None,
                committed: self,
                code,
            }),
        }
    }
}
impl CommittedPrivateRead {
    /// The validated payload.
    ///
    /// # Panics
    ///
    /// Never: a committed read holds bytes that already decoded for its scope.
    #[expect(
        clippy::expect_used,
        reason = "a CommittedPrivateRead is only built from bytes that decoded for its scope"
    )]
    pub fn payload(&self) -> &[u8] {
        codec::decode(&self.file.scope, &self.bytes)
            .expect("private validated read")
            .payload
    }
    /// The committed file's sequence.
    ///
    /// # Panics
    ///
    /// Never: a committed read always carries its validated cursor.
    #[expect(
        clippy::expect_used,
        reason = "a CommittedPrivateRead is only built after its cursor is validated"
    )]
    pub fn sequence(&self) -> u64 {
        self.file.cursor.expect("private validated cursor").sequence
    }
    pub fn release(self) -> PrivateActionsFile {
        self.file
    }
}
impl FailedPrivateRead {
    pub fn code(&self) -> SenderStorageCode {
        self.code
    }
    pub fn existing_commit_error(&self) -> Option<&ExistingCommitError> {
        self.existing.as_ref()
    }
    pub fn io_error(&self) -> Option<&io::Error> {
        self.error.as_ref().map(OpenError::io_error)
    }
    fn complete(mut self) -> PrivateReadOutcome {
        self.file.cursor = self.candidate;
        let result = (|| -> Result<(), OpenError> {
            let input = self.input.as_ref().ok_or(io::ErrorKind::InvalidData)?;
            self.file.cursor_identity = Some(tilo_files::native::atomic::FileIdentity::of(input)?);
            self.file.check_cursor().map(|_| ())
        })();
        if let Err(error) = result {
            self.error = Some(error);
            self.code = SenderStorageCode::Io;
            return PrivateReadOutcome::Failed(self);
        }
        let Some(input) = self.input.take() else {
            self.error = Some(OpenError::Io(io::ErrorKind::InvalidData.into()));
            self.code = SenderStorageCode::Io;
            return PrivateReadOutcome::Failed(self);
        };
        PrivateReadOutcome::Committed(CommittedPrivateRead {
            input,
            bytes: self.bytes,
            file: self.file,
        })
    }
    pub fn retry_commit(mut self) -> PrivateReadOutcome {
        let Some(error) = self.existing.take() else {
            return PrivateReadOutcome::Failed(self);
        };
        match error.retry_commit() {
            Ok(()) => self.complete(),
            Err(error) => {
                self.existing = Some(error);
                PrivateReadOutcome::Failed(self)
            }
        }
    }
}
impl CurrentPrivateState {
    pub fn workspace(&self) -> &str {
        self.file.workspace()
    }

    pub fn borrow_current(&self) -> Result<CurrentPrivateBorrow<'_>, SenderStorageCode> {
        if self.file.cursor != Some(self.stamp.cursor) || self.stamp.cache_revision != self.revision
        {
            return Err(SenderStorageCode::StaleCapture);
        }
        let view =
            self.file
                .cache
                .private_current(self.file.workspace(), self.revision, &self.stamp)?;
        Ok(CurrentPrivateBorrow {
            view,
            instance: self.stamp.cursor.instance,
            sequence: self.stamp.cursor.sequence,
        })
    }
    pub fn into_file(self) -> PrivateActionsFile {
        self.file
    }
    pub fn queue_owned(self, payload: Vec<u8>) -> Result<PrivateQueueOutcome, QueueRefusal> {
        let mut phase = QueueRefusal {
            state: self,
            payload,
            code: SenderStorageCode::StaleCapture,
        };
        if let Err(code) = phase.state.borrow_current().map(|_| ()) {
            phase.code = code;
            return Err(phase);
        }
        let result = phase.state.file.cache.private_queue(
            phase.state.file.workspace(),
            phase.state.revision,
            &phase.state.stamp,
            &mut phase.payload,
        );
        match result {
            Ok(Some(revision)) => {
                phase.state.file.selected_pending_revision = Some(revision);
                Ok(PrivateQueueOutcome::Pending(phase.state.file))
            }
            Ok(None) => {
                drop(phase.payload);
                Ok(PrivateQueueOutcome::Unchanged(phase.state))
            }
            Err(code) => {
                phase.code = code;
                Err(phase)
            }
        }
    }
}
impl CurrentPrivateBorrow<'_> {
    pub fn payload(&self) -> &[u8] {
        self.view.bytes()
    }
    pub fn instance(&self) -> &[u8; 16] {
        &self.instance
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
}
impl QueueRefusal {
    pub fn code(&self) -> SenderStorageCode {
        self.code
    }
    /// Returns the state and payload the refused queue kept.
    pub fn into_parts(self) -> (CurrentPrivateState, Vec<u8>) {
        (self.state, self.payload)
    }
}
impl HydrateRefusal {
    pub fn code(&self) -> SenderStorageCode {
        self.code
    }
    pub fn io_error(&self) -> Option<&io::Error> {
        self.error.as_ref().map(OpenError::io_error)
    }
    pub fn into_read(self) -> CommittedPrivateRead {
        self.read
    }
}
impl CommittedPrivateRead {
    /// The committed file's instance.
    ///
    /// # Panics
    ///
    /// Never: a committed read always carries its validated cursor.
    #[expect(
        clippy::expect_used,
        reason = "a CommittedPrivateRead is only built after its cursor is validated"
    )]
    pub fn instance(&self) -> &[u8; 16] {
        &self
            .file
            .cursor
            .as_ref()
            .expect("validated cursor")
            .instance
    }
    pub fn hydrate_current(self) -> Result<CurrentPrivateState, HydrateRefusal> {
        let mut phase = HydrateRefusal {
            read: self,
            error: None,
            code: SenderStorageCode::Capacity,
        };
        let fence = phase.read.file.check_cursor().and_then(|actual| {
            if actual != tilo_files::native::atomic::FileIdentity::of(&phase.read.input)? {
                return Err(io::ErrorKind::InvalidData.into());
            }
            Ok(())
        });
        if let Err(error) = fence {
            phase.error = Some(error);
            phase.code = SenderStorageCode::Io;
            return Err(phase);
        }
        let parsed = match codec::decode(&phase.read.file.scope, &phase.read.bytes) {
            Ok(v) => v,
            Err(code) => {
                phase.code = code;
                return Err(phase);
            }
        };
        let end = codec::HEADER + parsed.payload.len();
        let Some(stamp) = phase.read.file.stamp(parsed.payload, parsed.revision, 0) else {
            phase.code = SenderStorageCode::Io;
            return Err(phase);
        };
        let result = phase.read.file.cache.private_hydrate(
            phase.read.file.workspace(),
            &mut phase.read.bytes,
            codec::HEADER..end,
            stamp,
        );
        match result {
            Ok(()) => Ok(CurrentPrivateState {
                file: phase.read.file,
                revision: 0,
                stamp,
            }),
            Err(code) => {
                phase.code = code;
                Err(phase)
            }
        }
    }
}
impl PrepareRefusal {
    pub fn code(&self) -> SenderStorageCode {
        self.code
    }
    pub fn into_parts(self) -> (PrivateActionsFile, PrivateActionsCapture) {
        (self.file, self.capture)
    }
}
impl FinishRefusal {
    pub fn io_error(&self) -> Option<&io::Error> {
        self.error.as_ref().map(OpenError::io_error)
    }
    pub fn code(&self) -> SenderStorageCode {
        self.code
    }
    pub fn into_committed(self) -> CommittedPrivateWrite {
        self.committed
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
pub(crate) fn committed_test_fixture(
    payload: &[u8],
) -> (tempfile::TempDir, CurrentPrivateState, PortableStorage) {
    tests::consuming::actual_state_fixture(payload)
}
