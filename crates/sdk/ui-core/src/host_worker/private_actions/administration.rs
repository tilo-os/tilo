//! Closed administrative physical custody, without owner or sender authority.
//! The caller keeps its genuine owner lock. Prepared is already durable before
//! this carrier may create/acquire the sender sentinel. No cache is hydrated.
use super::{
    CHILD_NAME, FILE_NAME, LOCK_NAME, OpenError, OwnerDirectory, SenderScope,
    SenderStorageCode as Code, codec,
};
use crate::host_worker::managed_sender::{NativeSenderOutboxV1, OutboxFailure};
use std::{
    ffi::OsStr,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
};
use tilo_files::native::atomic::{
    ExistingCommitError, FileIdentity, PublicationError, PublicationState, PublishMode,
    StorageDirectory,
    bounded::FreshChildError,
    lease::{AcquireError, FileLease, LeaseMode},
};

/// Inert expected disk state; not evidence of a signed owner selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdministrativeExpectation {
    Prepared,
    Active,
}

/// Facts from this carrier's real existing-file barrier and current binding.
/// Fields have no public constructor and confer no owner/send permission.
#[derive(Debug, PartialEq, Eq)]
pub struct AdministrativeFacts {
    instance: [u8; 16],
    sequence: u64,
    revision: u64,
    digest: [u8; 32],
}
impl AdministrativeFacts {
    pub fn instance(&self) -> &[u8; 16] {
        &self.instance
    }
    pub fn sequence(&self) -> u64 {
        self.sequence
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn file_digest(&self) -> &[u8; 32] {
        &self.digest
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Admit,
    OpenSentinel,
    PublishSentinel,
    Acquire,
    SentinelBarrier,
    OpenChild,
    BindChild,
    PublishFile,
    Read,
    Validate,
    FileBarrier,
    Final,
}
enum PhysicalError {
    Publication(PublicationError),
    Existing(ExistingCommitError),
    Fresh(FreshChildError),
}
enum Cause {
    Open(OpenError),
    Physical(PhysicalError),
    Code(Code),
}
impl From<io::Error> for Cause {
    fn from(e: io::Error) -> Self {
        Self::Open(e.into())
    }
}
impl From<OpenError> for Cause {
    fn from(e: OpenError) -> Self {
        Self::Open(e)
    }
}
impl From<Code> for Cause {
    fn from(e: Code) -> Self {
        Self::Code(e)
    }
}
impl From<PublicationError> for Cause {
    fn from(e: PublicationError) -> Self {
        Self::Physical(PhysicalError::Publication(e))
    }
}
impl From<ExistingCommitError> for Cause {
    fn from(e: ExistingCommitError) -> Self {
        Self::Physical(PhysicalError::Existing(e))
    }
}
impl From<FreshChildError> for Cause {
    fn from(e: FreshChildError) -> Self {
        Self::Physical(PhysicalError::Fresh(e))
    }
}

struct Operation {
    physical: Option<PhysicalError>,
    diagnostic: Option<OpenError>,
    original: Option<StorageDirectory>,
    root: Option<OwnerDirectory>,
    root_identity: Option<FileIdentity>,
    sentinel: Option<File>,
    sentinel_identity: Option<FileIdentity>,
    lease: Option<FileLease>,
    child: Option<OwnerDirectory>,
    child_identity: Option<FileIdentity>,
    fresh_child: bool,
    input: Option<File>,
    input_identity: Option<FileIdentity>,
    initial: Vec<u8>,
    bytes: Vec<u8>,
    scope: SenderScope,
    instance: [u8; 16],
    expectation: AdministrativeExpectation,
    facts: Option<AdministrativeFacts>,
    step: Step,
}
pub struct AdministrativeFile {
    operation: Operation,
    facts: AdministrativeFacts,
}
pub struct AdministrativeFailure {
    operation: Operation,
    code: Code,
}
/// A current-check failure borrows the carrier: its original lease cannot be
/// released while this error (including a retained child FD) remains alive.
pub struct AdministrativeCheckFailure<'a> {
    error: OpenError,
    _file: &'a AdministrativeFile,
}
impl std::fmt::Debug for AdministrativeFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdministrativeFile")
            .field("facts", &self.facts)
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for AdministrativeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdministrativeFailure")
            .field("code", &self.code)
            .field("step", &self.operation.step)
            .field("io_error", &self.io_error())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for AdministrativeCheckFailure<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdministrativeCheckFailure")
            .field("io_error", &self.io_error())
            .finish_non_exhaustive()
    }
}
impl AdministrativeCheckFailure<'_> {
    pub fn io_error(&self) -> &io::Error {
        self.error.io_error()
    }
    pub fn code(&self) -> Code {
        io_code(self.io_error())
    }
}
fn io_code(error: &io::Error) -> Code {
    if error.kind() == io::ErrorKind::InvalidData {
        Code::InvalidFile
    } else {
        Code::Io
    }
}
impl AdministrativeFailure {
    pub fn code(&self) -> Code {
        self.code
    }
    pub fn io_error(&self) -> Option<&io::Error> {
        self.operation
            .diagnostic
            .as_ref()
            .map(OpenError::io_error)
            .or_else(|| match self.operation.physical.as_ref()? {
                PhysicalError::Publication(e) => Some(e.io_error()),
                PhysicalError::Existing(e) => Some(e.io_error()),
                PhysicalError::Fresh(e) => Some(e.io_error()),
            })
    }
    pub fn publication_error(&self) -> Option<&PublicationError> {
        match self.operation.physical.as_ref()? {
            PhysicalError::Publication(e) => Some(e),
            _ => None,
        }
    }
    pub fn existing_commit_error(&self) -> Option<&ExistingCommitError> {
        match self.operation.physical.as_ref()? {
            PhysicalError::Existing(e) => Some(e),
            _ => None,
        }
    }
    pub fn fresh_child_error(&self) -> Option<&FreshChildError> {
        match self.operation.physical.as_ref()? {
            PhysicalError::Fresh(e) => Some(e),
            _ => None,
        }
    }
    /// Retry under the original root/instance. Unknown publication
    /// and lost fresh-child custody remain errors, never new publication/adoption.
    pub fn retry(self) -> Result<AdministrativeFile, Self> {
        self.operation.run()
    }
}
impl Operation {
    fn root(&self) -> Result<&OwnerDirectory, OpenError> {
        self.root.as_ref().ok_or_else(Self::invalid)
    }
    fn child(&self) -> Result<&OwnerDirectory, OpenError> {
        self.child.as_ref().ok_or_else(Self::invalid)
    }
    fn invalid() -> OpenError {
        io::ErrorKind::InvalidData.into()
    }
    fn bindings(&self) -> Result<(), OpenError> {
        let Some(root) = &self.root else {
            return Ok(());
        };
        root.validate()?;
        let observed = root.identity()?;
        if self
            .root_identity
            .as_ref()
            .is_some_and(|id| id != &observed)
        {
            return Err(Self::invalid());
        }
        if let Some(identity) = &self.sentinel_identity {
            let named = root.open_lease_existing(OsStr::new(LOCK_NAME))?;
            if &FileIdentity::of(&named)? != identity {
                return Err(Self::invalid());
            }
            if let Some(lease) = &self.lease {
                if &lease.identity()? != identity {
                    return Err(Self::invalid());
                }
            }
        }
        if let Some(identity) = &self.child_identity {
            self.child()?.validate()?;
            let named = root
                .open_directory_child(OsStr::new(CHILD_NAME))
                .map_err(OpenError::Child)?;
            if &self.child()?.identity()? != identity || &named.identity()? != identity {
                return Err(Self::invalid());
            }
        }
        if let Some(identity) = &self.input_identity {
            let named = self.child()?.open_read(OsStr::new(FILE_NAME))?;
            if &FileIdentity::of(&named)? != identity
                || self
                    .input
                    .as_ref()
                    .map(FileIdentity::of)
                    .transpose()?
                    .as_ref()
                    != Some(identity)
            {
                return Err(Self::invalid());
            }
        }
        Ok(())
    }
    fn failed(mut self, cause: Cause) -> AdministrativeFailure {
        let code = match cause {
            Cause::Code(code) => code,
            Cause::Open(error) => {
                let code = io_code(error.io_error());
                self.diagnostic = Some(error);
                code
            }
            Cause::Physical(error) => {
                let code = match &error {
                    PhysicalError::Publication(e)
                        if e.state() == PublicationState::NotPublished =>
                    {
                        Code::NotPublished
                    }
                    PhysicalError::Publication(_) | PhysicalError::Existing(_) => Code::Uncertain,
                    PhysicalError::Fresh(_) => Code::Io,
                };
                self.physical = Some(error);
                code
            }
        };
        AdministrativeFailure {
            operation: self,
            code,
        }
    }
    fn retry_custody(&mut self) -> Result<(), Cause> {
        // Failed admission still owns the original FD; reclaim only that FD.
        if let Some(OpenError::Admission(error)) = self.diagnostic.take() {
            self.original = Some(error.into_directory());
        }
        self.bindings()?;
        match self.physical.take() {
            None => (),
            Some(PhysicalError::Publication(error)) => {
                match error.state() {
                    PublicationState::NotPublished => {
                        drop(error);
                    } // proven no effect, same exact proposal
                    PublicationState::PublicationUnknown => return Err(error.into()),
                    PublicationState::PublishedDurabilityUnknown => {
                        error.retry_commit()?;
                        self.step = match self.step {
                            Step::PublishSentinel => Step::OpenSentinel,
                            Step::PublishFile => Step::Read,
                            _ => return Err(Code::InvalidFile.into()),
                        };
                    }
                }
            }
            Some(PhysicalError::Existing(error)) => {
                error.retry_commit()?;
                self.step = match self.step {
                    Step::SentinelBarrier => Step::OpenChild,
                    Step::FileBarrier => Step::Final,
                    _ => return Err(Code::InvalidFile.into()),
                };
            }
            Some(PhysicalError::Fresh(error)) => {
                self.child = Some(error.retry_normalize_and_commit()?);
                self.fresh_child = true;
                self.step = Step::BindChild;
            }
        }
        Ok(())
    }
    fn run(mut self) -> Result<AdministrativeFile, AdministrativeFailure> {
        let preflight = (|| {
            if !matches!(self.scope.profile, 1 | 2) || self.instance == [0; 16] {
                return Err(Code::InvalidScope);
            }
            Ok(())
        })();
        if let Err(code) = preflight {
            return Err(self.failed(code.into()));
        }
        if let Err(error) = self.retry_custody() {
            return Err(self.failed(error));
        }
        loop {
            if self.step == Step::Final {
                let Some(facts) = self.facts.as_ref() else {
                    return Err(self.failed(Self::invalid().into()));
                };
                if let Err(error) = self.check_current(facts) {
                    return Err(self.failed(error.into()));
                }
                let Some(facts) = self.facts.take() else {
                    return Err(self.failed(Self::invalid().into()));
                };
                return Ok(AdministrativeFile {
                    operation: self,
                    facts,
                });
            }
            if let Err(error) = self.advance() {
                return Err(self.failed(error));
            }
        }
    }
    fn advance(&mut self) -> Result<(), Cause> {
        self.bindings()?;
        match self.step {
            Step::Admit => {
                if self.root.is_none() {
                    let original = self.original.take().ok_or_else(Self::invalid)?;
                    let root = OwnerDirectory::admit(original).map_err(OpenError::Admission)?;
                    self.root = Some(root);
                }
                self.root_identity = Some(self.root()?.identity()?);
                self.step = Step::OpenSentinel;
            }
            Step::OpenSentinel => match self.root()?.open_lease_existing(OsStr::new(LOCK_NAME)) {
                Ok(file) => {
                    let identity = FileIdentity::of(&file)?;
                    if self
                        .sentinel_identity
                        .as_ref()
                        .is_some_and(|expected| expected != &identity)
                    {
                        return Err(Self::invalid().into());
                    }
                    self.sentinel_identity = Some(identity);
                    self.sentinel = Some(file);
                    self.step = Step::Acquire;
                }
                Err(e)
                    if e.kind() == io::ErrorKind::NotFound
                        && self.expectation == AdministrativeExpectation::Prepared
                        && self.sentinel_identity.is_none() =>
                {
                    self.step = Step::PublishSentinel
                }
                Err(e) => return Err(e.into()),
            },
            Step::PublishSentinel => {
                match self
                    .root()?
                    .publish(OsStr::new(LOCK_NAME), &[], PublishMode::IfAbsent)
                {
                    Ok(()) => (),
                    Err(e)
                        if e.state() == PublicationState::NotPublished
                            && e.io_error().kind() == io::ErrorKind::AlreadyExists => {}
                    Err(e) => return Err(e.into()),
                }
                self.step = Step::OpenSentinel;
            }
            Step::Acquire => {
                self.step = Step::OpenSentinel; // Busy retry must reopen/check the same inode.
                match FileLease::try_acquire(
                    self.sentinel.take().ok_or_else(Self::invalid)?,
                    LeaseMode::Exclusive,
                ) {
                    Ok(lease) => self.lease = Some(lease),
                    Err(AcquireError::Busy) => return Err(Code::Busy.into()),
                    Err(AcquireError::Io(e)) => return Err(e.into()),
                }
                self.step = Step::SentinelBarrier;
            }
            Step::SentinelBarrier => {
                self.root()?.commit_existing(
                    OsStr::new(LOCK_NAME),
                    self.sentinel_identity.as_ref().ok_or_else(Self::invalid)?,
                )?;
                self.step = Step::OpenChild;
            }
            Step::OpenChild => {
                match self.root()?.open_directory_child(OsStr::new(CHILD_NAME)) {
                    Ok(child) => {
                        self.child = Some(child);
                        self.fresh_child = false;
                    }
                    Err(error)
                        if matches!(&error, tilo_files::native::atomic::bounded::ChildOpenError::Open(e) if e.kind() == io::ErrorKind::NotFound)
                            && self.expectation == AdministrativeExpectation::Prepared =>
                    {
                        let fresh = self.root()?.create_fresh_child(OsStr::new(CHILD_NAME))?;
                        self.child = Some(fresh.normalize_and_commit()?);
                        self.fresh_child = true;
                    }
                    Err(error) => return Err(OpenError::Child(error).into()),
                }
                self.step = Step::BindChild;
            }
            Step::BindChild => {
                self.child_identity = Some(self.child()?.identity()?);
                self.bindings()?;
                self.step = if self.fresh_child {
                    Step::PublishFile
                } else {
                    Step::Read
                };
            }
            Step::PublishFile => {
                if !self.fresh_child || self.expectation != AdministrativeExpectation::Prepared {
                    return Err(Code::InvalidFile.into());
                }
                if self.initial.is_empty() {
                    self.initial = codec::encode(
                        &self.scope,
                        self.instance,
                        1,
                        0,
                        NativeSenderOutboxV1::empty_bytes(),
                    )?;
                }
                self.child()?.publish(
                    OsStr::new(FILE_NAME),
                    &self.initial,
                    PublishMode::IfAbsent,
                )?;
                self.step = Step::Read;
            }
            Step::Read => {
                if self.input.is_none() {
                    self.input = Some(self.child()?.open_read(OsStr::new(FILE_NAME))?);
                }
                let Some(input) = self.input.as_mut() else {
                    return Err(Self::invalid().into());
                };
                self.input_identity = Some(FileIdentity::of(input)?);
                input.seek(SeekFrom::Start(0))?;
                if self.bytes.capacity() < codec::MAX_FILE + 1 {
                    self.bytes
                        .try_reserve_exact(codec::MAX_FILE + 1)
                        .map_err(|_| Code::Capacity)?;
                }
                self.bytes.resize(codec::MAX_FILE + 1, 0);
                let mut length = 0;
                while length < self.bytes.len() {
                    match input.read(&mut self.bytes[length..]) {
                        Ok(0) => break,
                        Ok(n) => length += n,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                self.bytes.truncate(length);
                self.step = Step::Validate;
            }
            Step::Validate => {
                let decoded = codec::decode(&self.scope, &self.bytes)?;
                if decoded.instance != self.instance {
                    return Err(Code::InvalidFile.into());
                }
                if self.expectation == AdministrativeExpectation::Prepared
                    && (decoded.sequence != 1
                        || decoded.revision != 0
                        || decoded.payload != NativeSenderOutboxV1::empty_bytes())
                {
                    return Err(Code::InvalidFile.into());
                }
                NativeSenderOutboxV1::decode(decoded.payload).map_err(|e| match e {
                    OutboxFailure::Capacity => Code::Capacity,
                    _ => Code::InvalidFile,
                })?;
                self.facts = Some(AdministrativeFacts {
                    instance: decoded.instance,
                    sequence: decoded.sequence,
                    revision: decoded.revision,
                    digest: *blake3::hash(&self.bytes).as_bytes(),
                });
                self.step = Step::FileBarrier;
            }
            Step::FileBarrier => {
                self.child()?.commit_existing(
                    OsStr::new(FILE_NAME),
                    self.input_identity.as_ref().ok_or_else(Self::invalid)?,
                )?;
                self.step = Step::Final;
            }
            // `run` finishes the final step before it would advance again.
            Step::Final => {}
        }
        Ok(())
    }
    fn check_current(&self, facts: &AdministrativeFacts) -> Result<(), OpenError> {
        self.bindings()?;
        let mut input = self.child()?.open_read(OsStr::new(FILE_NAME))?;
        if self.input_identity.as_ref() != Some(&FileIdentity::of(&input)?) {
            return Err(Self::invalid());
        }
        let mut scratch = [0u8; 8192];
        let mut hash = blake3::Hasher::new();
        let mut length = 0usize;
        loop {
            let n = match input.read(&mut scratch) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                result => result?,
            };
            if n == 0 {
                break;
            }
            length = length.checked_add(n).ok_or(io::ErrorKind::InvalidData)?;
            if length > codec::MAX_FILE {
                return Err(Self::invalid());
            }
            hash.update(&scratch[..n]);
        }
        if length != self.bytes.len() || hash.finalize().as_bytes() != &facts.digest {
            return Err(Self::invalid());
        }
        self.bindings()
    }
}
impl AdministrativeFile {
    pub fn open(
        original: StorageDirectory,
        scope: SenderScope,
        instance: [u8; 16],
        expectation: AdministrativeExpectation,
    ) -> Result<Self, AdministrativeFailure> {
        Operation {
            physical: None,
            diagnostic: None,
            original: Some(original),
            root: None,
            root_identity: None,
            sentinel: None,
            sentinel_identity: None,
            lease: None,
            child: None,
            child_identity: None,
            fresh_child: false,
            input: None,
            input_identity: None,
            initial: Vec::new(),
            bytes: Vec::new(),
            scope,
            instance,
            expectation,
            facts: None,
            step: Step::Admit,
        }
        .run()
    }
    /// Recheck exact current file and namespace while this carrier holds its
    /// actual Exclusive lease. Both returned facts and errors borrow this guard.
    pub fn revalidate(&self) -> Result<&AdministrativeFacts, AdministrativeCheckFailure<'_>> {
        self.operation
            .check_current(&self.facts)
            .map_err(|error| AdministrativeCheckFailure { error, _file: self })?;
        Ok(&self.facts)
    }
    /// Consume this same carrier when an enclosing operation must retain an
    /// owned refusal. The original Final phase preserves its exact facts,
    /// current-check error/failed FD and lease without new allocation.
    pub fn revalidate_owned(self) -> Result<Self, AdministrativeFailure> {
        let Self {
            mut operation,
            facts,
        } = self;
        operation.facts = Some(facts);
        operation.run()
    }
}

#[cfg(test)]
mod tests;
