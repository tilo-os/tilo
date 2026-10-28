//! Bounded bridge to the existing filesystem capability on a connected fd.
//! The Cap'n Proto client stays on its `LocalSet`; FUSE never receives source fds.

use crate::{Metadata, OpenMode};
use std::{
    collections::HashMap,
    io,
    os::unix::net::UnixStream,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::JoinHandle,
    time::Duration,
};

const LIMIT: usize = 16;
const HANDLES: usize = 256;
const METADATA_PINS: usize = 4096;
pub const ROOT_DIRECTORY: u64 = 1;
const DEADLINE: Duration = Duration::from_secs(10);
use crate::{
    EntryName,
    directory_client::{
        DirectoryClient, DirectoryCursorClient, MetadataPinClient, NativeJobClient,
    },
    directory_types::{DirectoryRows, DirectoryStat, DirectoryStreamPage, JobPoll},
};

enum Request {
    HistoryQuery(u64, EntryName, [u64; 2], Vec<u8>),
    HistoryReadSubject(u64, EntryName),
    CheckHistoryRead(u64, bool),
    ObserveHistoryRead(u64),
    ArmHistoryRead(u64, [u8; 32]),
    RetireHistoryReadHandoff(u64, [u8; 16]),
    ReleaseHistoryRead(u64),
    CheckPin(u64),
    CheckJob(u64),
    Check(u64),
    OpenCursor(u64),
    CursorPage(u64, u64),
    ReleaseCursor(u64),
    Metadata(u64),
    Stat(u64, EntryName),
    Readlink(u64, EntryName, [u64; 2]),
    Statfs(u64),
    SyncDirectory(u64),
    Pin(u64, EntryName, [u64; 2]),
    PinStat(u64),
    ReleasePin(u64),
    List(u64, u64),
    Directory(u64, EntryName),
    Open(u64, EntryName, OpenMode, Option<[u64; 2]>),
    Create(u64, EntryName, bool),
    RetireCreated(u64, EntryName, [u64; 2]),
    SetCreatedPermissions(u64, EntryName, [u64; 2], u32),
    SetTemporaryPermissions(u64, u32),
    Rename(u64, EntryName, EntryName),
    RenameReplace(u64, EntryName, EntryName),
    AtomicSave(u64, EntryName, [u64; 2], EntryName),
    Recoveries(u64, Vec<u8>),
    RestoreCaptured(u64, [u8; 16], EntryName),
    Poll(u64),
    Cancel(u64),
    ReleaseDirectory(u64),
    ReleaseJob(u64),
    PrepareReplacement(u64, EntryName, [u64; 2], EntryName, [u64; 2]),
    ObservePendingReplacement(u64),
    AcknowledgedCommittedPin(u64),
    AcknowledgedCommittedJob(u64),
    ArmPendingHandoff(u64, [u8; 32]),
    RetirePendingHandoff(u64, [u8; 16]),
    CheckPendingReplacement(u64),
    StartPendingReplacement(u64, Vec<u8>),
    CancelPendingReplacement(u64),
    ReleasePendingReplacement(u64),
    CreatedTemporary(u64),
    CheckCreatedTemporary(u64),
    RetireCreatedTemporary(u64),
    PrepareCreatedReplacement(u64),
    PrepareCreatedAtomicSave(u64),
    ReleaseCreatedTemporary(u64),
    ReplacementPin(u64),
    CheckReplacementPin(u64),
    CommitReplacementPin(u64, Vec<u8>),
    ObserveReplacementPin(u64),
    ReleaseReplacementPin(u64),
    HandleStat(u64),
    Read(u64, u64, u32),
    Write(u64, u64, Vec<u8>),
    Append(u64, Vec<u8>),
    Flush(u64),
    Release(u64),
}
enum Response {
    // Local alias availability only; true requires the actual remote scope check.
    HistoryReadCurrent(bool),
    AtomicPrepared(u64),
    AtomicRefused(String),
    AtomicUnknown(String),
    HistoryReadObservation(crate::directory_types::HistoryReadObservation),
    CursorPage(DirectoryStreamPage),
    Metadata(Metadata),
    Stat(Option<DirectoryStat>),
    Statfs(crate::directory_types::DirectoryFilesystemStats),
    DirectoryStat(DirectoryStat),
    Rows(DirectoryRows),
    Recoveries(crate::directory_types::RecoveryPage),
    Poll(JobPoll),
    ReplacementObservation(crate::directory_types::ReplacementObservation),
    PendingReplacementObservation(crate::directory_types::PendingReplacementObservation),
    Handle(u64),
    Bytes(Vec<u8>),
    Written(u32),
    Empty,
}
struct Call {
    request: Request,
    answer: mpsc::SyncSender<io::Result<Response>>,
}

struct Inner {
    calls: Option<tokio::sync::mpsc::Sender<Call>>,
    transport: UnixStream,
    live: Arc<AtomicBool>,
    fault: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<io::Result<()>>>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Release);
        drop(self.calls.take());
        let thread = match self.thread.get_mut() {
            Ok(thread) => thread,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(thread) = thread.take() {
            // Never in paint/layout: the projection owner retires off the UI.
            if let Err(error) = join_worker(thread) {
                eprintln!("retained source retirement failed: {error}");
            }
        }
    }
}

#[derive(Clone)]
pub struct DirectoryConnection(Arc<Inner>);

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Retained source is unavailable",
    )
}

fn join_worker(thread: JoinHandle<io::Result<()>>) -> io::Result<()> {
    thread
        .join()
        .map_err(|_| io::Error::other("retained source worker panicked"))?
}

impl DirectoryConnection {
    /// Close the queue and observe the exact RPC worker and transport outcome.
    /// Requires sole custody after all FUSE callbacks and handles have retired.
    pub fn retire(self) -> io::Result<()> {
        let mut inner = Arc::try_unwrap(self.0)
            .map_err(|_| io::Error::other("retained source still has active owners"))?;
        inner.live.store(false, Ordering::Release);
        drop(inner.calls.take());
        let thread = inner
            .thread
            .get_mut()
            .map_err(|_| io::Error::other("retained source worker lock poisoned"))?
            .take()
            .ok_or_else(|| io::Error::other("retained source already retired"))?;
        join_worker(thread)
    }
    /// Terminally close this exact authenticated channel before waiting for
    /// callbacks. The source provider observes peer death at native effect gates.
    pub fn revoke(&self) {
        self.0.live.store(false, Ordering::Release);
        let _ = self.0.transport.shutdown(std::net::Shutdown::Both);
    }
    pub fn connected(stream: UnixStream) -> io::Result<Self> {
        let transport = stream.try_clone()?;
        let (calls, receive) = tokio::sync::mpsc::channel::<Call>(LIMIT);
        let live = Arc::new(AtomicBool::new(true));
        let worker_live = Arc::clone(&live);
        let fault = Arc::new(AtomicBool::new(false));
        let worker_fault = Arc::clone(&fault);
        let (started, ready) = mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("retained-directory-fs".into())
            .spawn(move || run_worker(stream, receive, worker_live, worker_fault, started))?;
        let inner = Arc::new(Inner {
            calls: Some(calls),
            transport,
            live,
            fault,
            thread: Mutex::new(Some(thread)),
        });
        ready
            .recv_timeout(DEADLINE.saturating_add(Duration::from_secs(1)))
            .map_err(|_| refused())??;
        Ok(Self(inner))
    }
    fn call(&self, request: Request) -> io::Result<Response> {
        self.call_with(|| request)
    }
    fn call_with(&self, request: impl FnOnce() -> Request) -> io::Result<Response> {
        if !self.0.live.load(Ordering::Acquire) {
            return Err(refused());
        }
        let (answer, receive) = mpsc::sync_channel(1);
        let permit = self
            .0
            .calls
            .as_ref()
            .ok_or_else(refused)?
            .try_reserve()
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(()) => {
                    crate::native_reply::bounded_refusal()
                }
                tokio::sync::mpsc::error::TrySendError::Closed(()) => refused(),
            })?;
        // Finite admission precedes cloning a potentially 1 MiB payload.
        permit.send(Call {
            request: request(),
            answer,
        });
        receive
            .recv_timeout(DEADLINE.saturating_add(Duration::from_secs(1)))
            .map_err(|_| {
                self.0.live.store(false, Ordering::Release);
                self.0.fault.store(true, Ordering::Release);
                refused()
            })?
    }
    /// # Errors
    /// Refuses bounded cursor admission or authority/native IO failure.
    pub fn open_cursor(&self, directory: u64) -> io::Result<u64> {
        match self.call(Request::OpenCursor(directory))? {
            Response::Handle(id) => Ok(id),
            _ => Err(refused()),
        }
    }
    /// # Errors
    /// Refuses native enumeration, wrong sequence or authority/transport failure.
    pub fn cursor_page(&self, cursor: u64, sequence: u64) -> io::Result<DirectoryStreamPage> {
        match self.call(Request::CursorPage(cursor, sequence))? {
            Response::CursorPage(page) => Ok(page),
            _ => Err(refused()),
        }
    }
    pub fn release_cursor(&self, cursor: u64) -> io::Result<()> {
        match self.call(Request::ReleaseCursor(cursor))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    /// # Errors
    /// Refuses revoked source handle scope or transport loss.
    /// # Errors
    /// Refuses revoked source metadata custody or transport failure.
    pub fn check_pin(&self, id: u64) -> io::Result<()> {
        match self.call(Request::CheckPin(id))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    /// # Errors
    /// Refuses revoked native job scope or transport failure.
    pub fn check_job(&self, id: u64) -> io::Result<()> {
        match self.call(Request::CheckJob(id))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    pub fn check_scope(&self, id: u64) -> io::Result<()> {
        match self.call(Request::Check(id))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    /// Retain actual readable source custody; names never construct a grant.
    /// # Errors
    /// Refuses unsupported providers, missing Read, substitutions or source loss.
    pub fn history_query(
        &self,
        directory: u64,
        name: &EntryName,
        expected: [u64; 2],
        query: &[u8],
    ) -> io::Result<Vec<u8>> {
        crate::directory_types::validate_history_query(query).map_err(io::Error::other)?;
        if expected[1] == 0 {
            return Err(refused());
        }
        match self.call(Request::HistoryQuery(
            directory,
            name.clone(),
            expected,
            query.to_vec(),
        ))? {
            Response::Bytes(bytes) => Ok(bytes),
            _ => Err(refused()),
        }
    }
    /// # Errors
    /// Refuses unsupported providers, missing Read, substitutions or source loss.
    pub fn history_read_subject(&self, directory: u64, name: &EntryName) -> io::Result<u64> {
        match self.call(Request::HistoryReadSubject(directory, name.clone()))? {
            Response::Handle(id) => Ok(id),
            _ => Err(refused()),
        }
    }
    /// # Errors
    /// Refuses expired custody; named=true also brackets exact selected name.
    pub fn check_history_read(&self, id: u64, named: bool) -> io::Result<()> {
        match self.call(Request::CheckHistoryRead(id, named))? {
            Response::HistoryReadCurrent(true) => Ok(()),
            _ => Err(refused()),
        }
    }
    /// # Errors
    /// Refuses stale source authority or malformed native observation.
    pub fn observe_history_read(
        &self,
        id: u64,
    ) -> io::Result<crate::directory_types::HistoryReadObservation> {
        match self.call(Request::ObserveHistoryRead(id))? {
            Response::HistoryReadObservation(observation) => Ok(observation),
            _ => Err(refused()),
        }
    }
    pub fn retire_history_read_handoff(&self, id: u64, ticket: [u8; 16]) -> io::Result<()> {
        self.empty(Request::RetireHistoryReadHandoff(id, ticket))
    }
    pub fn arm_history_read_handoff(&self, id: u64, challenge: [u8; 32]) -> io::Result<[u8; 16]> {
        match self.call(Request::ArmHistoryRead(id, challenge))? {
            Response::Bytes(ticket) => ticket.try_into().map_err(|_| refused()),
            _ => Err(refused()),
        }
    }
    pub fn release_history_read(&self, id: u64) -> io::Result<()> {
        match self.call(Request::ReleaseHistoryRead(id))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    pub fn metadata(&self, directory: u64) -> io::Result<DirectoryStat> {
        match self.call(Request::Metadata(directory))? {
            Response::DirectoryStat(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    /// # Errors
    /// Refuses changed identity, type, bounded admission or authority/IO failure.
    pub fn pin_metadata(
        &self,
        directory: u64,
        name: &EntryName,
        expected: [u64; 2],
    ) -> io::Result<u64> {
        match self.call(Request::Pin(directory, name.clone(), expected))? {
            Response::Handle(id) => Ok(id),
            _ => Err(refused()),
        }
    }
    /// # Errors
    /// Refuses unavailable or revoked metadata custody.
    pub fn metadata_pin(&self, id: u64) -> io::Result<DirectoryStat> {
        match self.call(Request::PinStat(id))? {
            Response::DirectoryStat(stat) => Ok(stat),
            _ => Err(refused()),
        }
    }
    pub fn release_pin(&self, id: u64) -> io::Result<()> {
        match self.call(Request::ReleasePin(id))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    /// Actual held source filesystem metadata, without child byte authority.
    /// # Errors
    /// Refuses missing held capability, source revocation or actual fsync failure.
    pub fn sync_directory(&self, directory: u64) -> io::Result<()> {
        match self.call(Request::SyncDirectory(directory))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    pub fn statfs(
        &self,
        directory: u64,
    ) -> io::Result<crate::directory_types::DirectoryFilesystemStats> {
        match self.call(Request::Statfs(directory))? {
            Response::Statfs(stats) => Ok(stats),
            _ => Err(refused()),
        }
    }
    /// # Errors
    /// Refuses substituted links, revoked scope, or bounded transport failure.
    pub fn readlink(
        &self,
        directory: u64,
        name: &EntryName,
        expected: [u64; 2],
    ) -> io::Result<Vec<u8>> {
        match self.call(Request::Readlink(directory, name.clone(), expected))? {
            Response::Bytes(bytes) => Ok(bytes),
            _ => Err(refused()),
        }
    }
    pub fn stat(&self, directory: u64, name: &EntryName) -> io::Result<Option<DirectoryStat>> {
        match self.call(Request::Stat(directory, name.clone()))? {
            Response::Stat(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn list(&self, directory: u64, cursor: u64) -> io::Result<DirectoryRows> {
        match self.call(Request::List(directory, cursor))? {
            Response::Rows(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn directory(&self, directory: u64, name: &EntryName) -> io::Result<u64> {
        self.handle(Request::Directory(directory, name.clone()))
    }
    pub fn open(&self, directory: u64, name: &EntryName, mode: OpenMode) -> io::Result<u64> {
        self.handle(Request::Open(directory, name.clone(), mode, None))
    }
    pub fn open_expected(
        &self,
        directory: u64,
        name: &EntryName,
        mode: OpenMode,
        expected: [u64; 2],
    ) -> io::Result<u64> {
        self.handle(Request::Open(directory, name.clone(), mode, Some(expected)))
    }
    pub fn set_created_permissions(
        &self,
        directory: u64,
        name: &EntryName,
        expected: [u64; 2],
        mode: u32,
    ) -> io::Result<()> {
        if mode & !0o7777 != 0 {
            return Err(refused());
        }
        self.empty(Request::SetCreatedPermissions(
            directory,
            name.clone(),
            expected,
            mode,
        ))
    }
    pub fn set_temporary_permissions(&self, id: u64, mode: u32) -> io::Result<()> {
        if mode & !0o7777 != 0 {
            return Err(refused());
        }
        self.empty(Request::SetTemporaryPermissions(id, mode))
    }
    pub fn retire_created(
        &self,
        directory: u64,
        name: &EntryName,
        expected: [u64; 2],
    ) -> io::Result<u64> {
        match self.call(Request::RetireCreated(directory, name.clone(), expected))? {
            Response::Handle(job) => Ok(job),
            _ => Err(refused()),
        }
    }
    pub fn create_file(&self, directory: u64, name: &EntryName) -> io::Result<u64> {
        self.handle(Request::Create(directory, name.clone(), false))
    }
    pub fn create_directory(&self, directory: u64, name: &EntryName) -> io::Result<u64> {
        self.handle(Request::Create(directory, name.clone(), true))
    }

    pub fn recoveries(
        &self,
        directory: u64,
        after: &[u8],
    ) -> io::Result<crate::directory_types::RecoveryPage> {
        if !after.is_empty() && after.len() != 24 {
            return Err(crate::native_reply::bounded_refusal());
        }
        match self.call(Request::Recoveries(directory, after.to_vec()))? {
            Response::Recoveries(page) => Ok(page),
            _ => Err(refused()),
        }
    }
    pub fn restore_captured(
        &self,
        directory: u64,
        recovery: [u8; 16],
        name: &EntryName,
    ) -> io::Result<u64> {
        match self.call(Request::RestoreCaptured(directory, recovery, name.clone()))? {
            Response::Handle(job) => Ok(job),
            _ => Err(refused()),
        }
    }
    pub fn atomic_save(
        &self,
        directory: u64,
        temporary: &EntryName,
        expected: [u64; 2],
        target: &EntryName,
    ) -> io::Result<()> {
        match self.call(Request::AtomicSave(
            directory,
            temporary.clone(),
            expected,
            target.clone(),
        ))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    pub fn rename_replace(
        &self,
        directory: u64,
        name: &EntryName,
        new: &EntryName,
    ) -> io::Result<u64> {
        self.handle(Request::RenameReplace(directory, name.clone(), new.clone()))
    }
    pub fn rename_no_replace(
        &self,
        directory: u64,
        name: &EntryName,
        new: &EntryName,
    ) -> io::Result<u64> {
        self.handle(Request::Rename(directory, name.clone(), new.clone()))
    }
    /// Reserve genuine work under this connected source; wire IDs grant no rights.
    pub fn prepare_replacement(
        &self,
        parent: u64,
        temporary: &EntryName,
        expected: [u64; 2],
        victim: &EntryName,
        old: [u64; 2],
    ) -> io::Result<u64> {
        if temporary.as_os_str().len() > 4096 || victim.as_os_str().len() > 4096 {
            return Err(crate::native_reply::bounded_refusal());
        }
        self.handle(Request::PrepareReplacement(
            parent,
            temporary.clone(),
            expected,
            victim.clone(),
            old,
        ))
    }
    pub fn retire_pending_handoff(&self, id: u64, ticket: [u8; 16]) -> io::Result<()> {
        match self.call(Request::RetirePendingHandoff(id, ticket))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    pub fn arm_pending_handoff(&self, id: u64, challenge: [u8; 32]) -> io::Result<[u8; 16]> {
        match self.call(Request::ArmPendingHandoff(id, challenge))? {
            Response::Bytes(bytes) => bytes.as_slice().try_into().map_err(|_| refused()),
            _ => Err(refused()),
        }
    }
    pub fn acknowledged_committed_job(&self, pending: u64) -> io::Result<u64> {
        self.handle(Request::AcknowledgedCommittedJob(pending))
    }
    pub fn acknowledged_committed_pin(&self, pending: u64) -> io::Result<u64> {
        self.handle(Request::AcknowledgedCommittedPin(pending))
    }
    pub fn observe_pending_replacement(
        &self,
        pending: u64,
    ) -> io::Result<crate::directory_types::PendingReplacementObservation> {
        match self.call(Request::ObservePendingReplacement(pending))? {
            Response::PendingReplacementObservation(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn check_pending_replacement(&self, pending: u64) -> io::Result<()> {
        self.empty(Request::CheckPendingReplacement(pending))
    }
    pub fn start_pending_replacement(&self, pending: u64, receipt: &[u8]) -> io::Result<u64> {
        if receipt.is_empty() || receipt.len() > crate::external_history::LIMIT {
            return Err(crate::native_reply::bounded_refusal());
        }
        self.handle(Request::StartPendingReplacement(pending, receipt.to_vec()))
    }
    pub fn cancel_pending_replacement(&self, pending: u64) -> io::Result<()> {
        self.empty(Request::CancelPendingReplacement(pending))
    }
    pub fn release_pending_replacement(&self, pending: u64) -> io::Result<()> {
        self.empty(Request::ReleasePendingReplacement(pending))
    }
    /// Retain actual remote capture custody for a completed genuine job.
    pub fn pin_replacement(&self, job: u64) -> io::Result<u64> {
        match self.call(Request::ReplacementPin(job))? {
            Response::Handle(id) => Ok(id),
            _ => Err(refused()),
        }
    }
    pub fn created_temporary(&self, job: u64) -> io::Result<u64> {
        match self.call(Request::CreatedTemporary(job))? {
            Response::Handle(id) => Ok(id),
            _ => Err(refused()),
        }
    }
    pub fn check_created_temporary(&self, id: u64) -> io::Result<()> {
        self.empty(Request::CheckCreatedTemporary(id))
    }
    pub fn prepare_created_atomic_save(
        &self,
        id: u64,
    ) -> crate::directory_types::AtomicSavePreparation<u64> {
        use crate::directory_types::AtomicSavePreparation as Outcome;
        match self.call(Request::PrepareCreatedAtomicSave(id)) {
            Ok(Response::AtomicPrepared(id)) => Outcome::Prepared(id),
            Ok(Response::AtomicRefused(error)) => Outcome::Refused(error),
            Ok(Response::AtomicUnknown(error)) => Outcome::Unknown(error),
            Ok(_) => Outcome::Unknown("Unexpected AtomicSave preparation response".into()),
            Err(error) => Outcome::Unknown(error.to_string()),
        }
    }
    pub fn prepare_created_replacement(&self, id: u64) -> io::Result<u64> {
        match self.call(Request::PrepareCreatedReplacement(id))? {
            Response::Handle(pending) => Ok(pending),
            _ => Err(refused()),
        }
    }
    pub fn retire_created_temporary(&self, id: u64) -> io::Result<u64> {
        match self.call(Request::RetireCreatedTemporary(id))? {
            Response::Handle(job) => Ok(job),
            _ => Err(refused()),
        }
    }
    pub fn release_created_temporary(&self, id: u64) -> io::Result<()> {
        self.empty(Request::ReleaseCreatedTemporary(id))
    }
    pub fn commit_replacement_pin(&self, pin: u64, receipt: &[u8]) -> io::Result<()> {
        if receipt.is_empty() || receipt.len() > crate::external_history::LIMIT {
            return Err(refused());
        }
        self.empty(Request::CommitReplacementPin(pin, receipt.to_vec()))
    }
    pub fn check_replacement_pin(&self, pin: u64) -> io::Result<()> {
        self.empty(Request::CheckReplacementPin(pin))
    }
    pub fn observe_replacement_pin(
        &self,
        pin: u64,
    ) -> io::Result<crate::directory_types::ReplacementObservation> {
        match self.call(Request::ObserveReplacementPin(pin))? {
            Response::ReplacementObservation(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn release_replacement_pin(&self, pin: u64) -> io::Result<()> {
        self.empty(Request::ReleaseReplacementPin(pin))
    }
    pub fn poll(&self, job: u64) -> io::Result<JobPoll> {
        match self.call(Request::Poll(job))? {
            Response::Poll(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn cancel(&self, job: u64) -> io::Result<()> {
        self.empty(Request::Cancel(job))
    }
    pub fn release_directory(&self, directory: u64) -> io::Result<()> {
        self.empty(Request::ReleaseDirectory(directory))
    }
    pub fn release_job(&self, job: u64) -> io::Result<()> {
        self.empty(Request::ReleaseJob(job))
    }
    fn handle(&self, request: Request) -> io::Result<u64> {
        match self.call(request)? {
            Response::Handle(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    fn empty(&self, request: Request) -> io::Result<()> {
        match self.call(request)? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    pub fn metadata_handle(&self, id: u64) -> io::Result<Metadata> {
        match self.call(Request::HandleStat(id))? {
            Response::Metadata(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn read(&self, id: u64, offset: u64, len: u32) -> io::Result<Vec<u8>> {
        if len > crate::MAX_READ_CHUNK {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        match self.call(Request::Read(id, offset, len))? {
            Response::Bytes(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn write(&self, id: u64, offset: u64, bytes: &[u8]) -> io::Result<u32> {
        if bytes.len() > crate::MAX_WRITE_CHUNK as usize {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        match self.call_with(|| Request::Write(id, offset, bytes.to_vec()))? {
            Response::Written(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    /// One non-idempotent append; a failed transport may have an unknown effect.
    pub fn append(&self, id: u64, bytes: &[u8]) -> io::Result<u32> {
        if bytes.len() > crate::MAX_WRITE_CHUNK as usize {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        match self.call_with(|| Request::Append(id, bytes.to_vec()))? {
            Response::Written(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn flush(&self, id: u64) -> io::Result<()> {
        match self.call(Request::Flush(id))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    pub fn release(&self, id: u64) -> io::Result<()> {
        match self.call(Request::Release(id))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
}

fn room(count: usize, next: u64) -> io::Result<()> {
    if count >= HANDLES || next == u64::MAX {
        Err(io::Error::other("retained directory capability limit"))
    } else {
        Ok(())
    }
}

// Boxed futures deliberately omit Send: capability clients stay on the local worker task.
type LocalFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;

// Capability clients remain confined to the worker's local task throughout each request.
struct WorkerState {
    directories: HashMap<u64, DirectoryClient>,
    handles: HashMap<u64, crate::FileHandle>,
    jobs: HashMap<u64, NativeJobClient>,
    replacement_pins: HashMap<u64, crate::directory_client::ReplacementPinClient>,
    created_temporaries: HashMap<u64, crate::directory_client::CreatedTemporaryClient>,
    cursors: HashMap<u64, DirectoryCursorClient>,
    pins: HashMap<u64, MetadataPinClient>,
    history_reads: HashMap<u64, crate::directory_client::HistoryReadSubjectClient>,
    pending_replacements: HashMap<u64, crate::directory_client::PendingReplacementClient>,
    next: u64,
}
impl WorkerState {
    fn history_query(
        &self,
        id: u64,
        name: EntryName,
        expected: [u64; 2],
        query: Vec<u8>,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.directories
                .get(&id)
                .ok_or_else(refused)?
                .history_query(&name, expected, &query)
                .await
                .map(Response::Bytes)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn history_read_subject(
        &mut self,
        id: u64,
        name: EntryName,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.history_reads.len() >= 64 {
                return Err(crate::native_reply::bounded_refusal());
            }
            if self.next == u64::MAX {
                return Err(refused());
            }
            let subject = self
                .directories
                .get(&id)
                .ok_or_else(refused)?
                .history_read_subject(&name)
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.history_reads.insert(self.next, subject);
            Ok(Response::Handle(self.next))
        })
    }
    fn check_history_read(
        &mut self,
        id: u64,
        named: bool,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            // A missing local alias has no authority to check. Its
            // denial must not close unrelated live capabilities.
            let Some(subject) = self.history_reads.get(&id) else {
                return Ok(Response::HistoryReadCurrent(false));
            };
            if named {
                subject.check().await
            } else {
                subject.check_scope().await
            }
            .map_err(crate::native_reply::client_error)?;
            Ok(Response::HistoryReadCurrent(true))
        })
    }
    fn retire_history_read_handoff(
        &mut self,
        id: u64,
        ticket: [u8; 16],
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.history_reads
                .get(&id)
                .ok_or_else(refused)?
                .retire_daemon_handoff(ticket)
                .await
                .map(|()| Response::Empty)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn arm_history_read(
        &mut self,
        id: u64,
        challenge: [u8; 32],
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.history_reads
                .get(&id)
                .ok_or_else(refused)?
                .arm_daemon_handoff(challenge)
                .await
                .map(|ticket| Response::Bytes(ticket.to_vec()))
                .map_err(crate::native_reply::client_error)
        })
    }
    fn observe_history_read(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.history_reads
                .get(&id)
                .ok_or_else(refused)?
                .observe()
                .await
                .map(Response::HistoryReadObservation)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn release_history_read(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.history_reads.remove(&id).ok_or_else(refused)?;
            Ok(Response::Empty)
        })
    }
    fn check_pin(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.pins
                .get(&id)
                .ok_or_else(refused)?
                .check_scope()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn check_job(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.jobs
                .get(&id)
                .ok_or_else(refused)?
                .check_scope()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn check(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .check_scope()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn open_cursor(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            room(
                self.directories
                    .len()
                    .saturating_add(self.handles.len())
                    .saturating_add(self.jobs.len())
                    .saturating_add(self.cursors.len()),
                self.next,
            )?;
            if self.cursors.len() >= 64 {
                return Err(crate::native_reply::bounded_refusal());
            }
            let cursor = self
                .directories
                .get(&id)
                .ok_or_else(refused)?
                .open_cursor()
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.cursors.insert(self.next, cursor);
            Ok(Response::Handle(self.next))
        })
    }
    fn cursor_page(&mut self, id: u64, sequence: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.cursors
                .get(&id)
                .ok_or_else(refused)?
                .page(sequence)
                .await
                .map(Response::CursorPage)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn release_cursor(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.cursors.remove(&id).ok_or_else(refused)?;
            Ok(Response::Empty)
        })
    }
    fn pin(
        &mut self,
        id: u64,
        name: EntryName,
        expected: [u64; 2],
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.pins.len() >= METADATA_PINS || self.next == u64::MAX {
                return Err(crate::native_reply::bounded_refusal());
            }
            let pin = self
                .directories
                .get(&id)
                .ok_or_else(refused)?
                .pin_metadata(&name, expected)
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.pins.insert(self.next, pin);
            Ok(Response::Handle(self.next))
        })
    }
    fn pin_stat(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.pins
                .get(&id)
                .ok_or_else(refused)?
                .metadata()
                .await
                .map(Response::DirectoryStat)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn release_pin(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.pins.remove(&id).ok_or_else(refused)?;
            Ok(Response::Empty)
        })
    }
    fn handle_stat(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .metadata()
                .await
                .map(Response::Metadata)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn metadata(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.directories
                .get(&id)
                .ok_or_else(refused)?
                .metadata()
                .await
                .map(Response::DirectoryStat)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn sync_directory(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.directories
                .get(&id)
                .ok_or_else(refused)?
                .sync_directory()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn statfs(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.directories
                .get(&id)
                .ok_or_else(refused)?
                .statfs()
                .await
                .map(Response::Statfs)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn readlink(
        &mut self,
        id: u64,
        name: EntryName,
        expected: [u64; 2],
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.directories
                .get(&id)
                .ok_or_else(refused)?
                .readlink(&name, expected)
                .await
                .map(Response::Bytes)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn stat(&self, id: u64, name: EntryName) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.directories
                .get(&id)
                .ok_or_else(refused)?
                .stat(&name)
                .await
                .map(Response::Stat)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn list(&mut self, id: u64, cursor: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.directories
                .get(&id)
                .ok_or_else(refused)?
                .list(cursor)
                .await
                .map(Response::Rows)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn directory(&mut self, id: u64, name: EntryName) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            room(
                self.directories
                    .len()
                    .saturating_add(self.handles.len())
                    .saturating_add(self.jobs.len())
                    .saturating_add(self.cursors.len()),
                self.next,
            )?;
            let child = self
                .directories
                .get(&id)
                .ok_or_else(refused)?
                .directory(&name)
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.directories.insert(self.next, child);
            Ok(Response::Handle(self.next))
        })
    }
    fn open(
        &mut self,
        id: u64,
        name: EntryName,
        mode: OpenMode,
        expected: Option<[u64; 2]>,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            room(
                self.directories
                    .len()
                    .saturating_add(self.handles.len())
                    .saturating_add(self.jobs.len())
                    .saturating_add(self.cursors.len()),
                self.next,
            )?;
            let directory = self.directories.get(&id).ok_or_else(refused)?;
            let handle = match expected {
                Some(expected) => directory.open_file_expected(&name, mode, expected).await,
                None => directory.open_file(&name, mode).await,
            }
            .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.handles.insert(self.next, handle);
            Ok(Response::Handle(self.next))
        })
    }
    fn create(
        &mut self,
        id: u64,
        name: EntryName,
        folder: bool,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            room(
                self.directories
                    .len()
                    .saturating_add(self.handles.len())
                    .saturating_add(self.jobs.len())
                    .saturating_add(self.cursors.len()),
                self.next,
            )?;
            let directory = self.directories.get(&id).ok_or_else(refused)?;
            let job = if folder {
                directory.create_directory(&name).await
            } else {
                directory.create_file(&name).await
            }
            .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.jobs.insert(self.next, job);
            Ok(Response::Handle(self.next))
        })
    }
    fn recoveries(&self, id: u64, after: Vec<u8>) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.directories
                .get(&id)
                .ok_or_else(refused)?
                .recoveries(&after)
                .await
                .map(Response::Recoveries)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn restore_captured(
        &mut self,
        id: u64,
        recovery: [u8; 16],
        name: EntryName,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            room(
                self.directories
                    .len()
                    .saturating_add(self.handles.len())
                    .saturating_add(self.jobs.len())
                    .saturating_add(self.pins.len())
                    .saturating_add(self.cursors.len()),
                self.next,
            )?;
            let job = self
                .directories
                .get(&id)
                .ok_or_else(refused)?
                .restore_captured(recovery, &name)
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.jobs.insert(self.next, job);
            Ok(Response::Handle(self.next))
        })
    }
    fn atomic_save(
        &self,
        id: u64,
        temporary: EntryName,
        expected: [u64; 2],
        target: EntryName,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.directories
                .get(&id)
                .ok_or_else(refused)?
                .atomic_save(&temporary, expected, &target)
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn rename_replace(
        &mut self,
        id: u64,
        name: EntryName,
        new: EntryName,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            room(
                self.directories
                    .len()
                    .saturating_add(self.handles.len())
                    .saturating_add(self.jobs.len())
                    .saturating_add(self.cursors.len()),
                self.next,
            )?;
            let job = self
                .directories
                .get(&id)
                .ok_or_else(refused)?
                .rename_replace(&name, &new)
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.jobs.insert(self.next, job);
            Ok(Response::Handle(self.next))
        })
    }
    fn rename(
        &mut self,
        id: u64,
        name: EntryName,
        new: EntryName,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            room(
                self.directories
                    .len()
                    .saturating_add(self.handles.len())
                    .saturating_add(self.jobs.len())
                    .saturating_add(self.cursors.len()),
                self.next,
            )?;
            let job = self
                .directories
                .get(&id)
                .ok_or_else(refused)?
                .rename_no_replace(&name, &new)
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.jobs.insert(self.next, job);
            Ok(Response::Handle(self.next))
        })
    }
    fn prepare_replacement(
        &mut self,
        id: u64,
        temporary: EntryName,
        expected: [u64; 2],
        victim: EntryName,
        old: [u64; 2],
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.pending_replacements.len() >= 64 || self.next == u64::MAX {
                return Err(crate::native_reply::bounded_refusal());
            }
            let pending = self
                .directories
                .get(&id)
                .ok_or_else(refused)?
                .prepare_replace(&temporary, expected, &victim, old)
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.pending_replacements.insert(self.next, pending);
            Ok(Response::Handle(self.next))
        })
    }
    fn retire_pending_handoff(
        &mut self,
        id: u64,
        ticket: [u8; 16],
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.pending_replacements
                .get(&id)
                .ok_or_else(refused)?
                .retire_daemon_handoff(ticket)
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn arm_pending_handoff(
        &mut self,
        id: u64,
        challenge: [u8; 32],
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.pending_replacements
                .get(&id)
                .ok_or_else(refused)?
                .arm_daemon_handoff(&challenge)
                .await
                .map(|ticket| Response::Bytes(ticket.to_vec()))
                .map_err(crate::native_reply::client_error)
        })
    }
    fn acknowledged_committed_job(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.jobs.len() >= 64 || self.next == u64::MAX {
                return Err(crate::native_reply::bounded_refusal());
            }
            let job = self
                .pending_replacements
                .get(&id)
                .ok_or_else(refused)?
                .acknowledged_committed_job()
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.jobs.insert(self.next, job);
            Ok(Response::Handle(self.next))
        })
    }
    fn acknowledged_committed_pin(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.replacement_pins.len() >= 64 || self.next == u64::MAX {
                return Err(crate::native_reply::bounded_refusal());
            }
            let pin = self
                .pending_replacements
                .get(&id)
                .ok_or_else(refused)?
                .acknowledged_committed_pin()
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.replacement_pins.insert(self.next, pin);
            Ok(Response::Handle(self.next))
        })
    }
    fn observe_pending_replacement(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.pending_replacements
                .get(&id)
                .ok_or_else(refused)?
                .observe()
                .await
                .map(Response::PendingReplacementObservation)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn check_pending_replacement(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.pending_replacements
                .get(&id)
                .ok_or_else(refused)?
                .check_scope()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn start_pending_replacement(
        &mut self,
        id: u64,
        receipt: Vec<u8>,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            room(
                self.directories
                    .len()
                    .saturating_add(self.handles.len())
                    .saturating_add(self.jobs.len())
                    .saturating_add(self.pins.len())
                    .saturating_add(self.cursors.len()),
                self.next,
            )?;
            let job = self
                .pending_replacements
                .get(&id)
                .ok_or_else(refused)?
                .start_with_history(&receipt)
                .await
                .map_err(crate::native_reply::client_error)?;
            // Retain the same Pending for source-owned committed ACK.
            // A lost response is unknown and no raw-start replay is attempted.
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.jobs.insert(self.next, job);
            Ok(Response::Handle(self.next))
        })
    }
    fn cancel_pending_replacement(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.pending_replacements
                .get(&id)
                .ok_or_else(refused)?
                .cancel()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn release_pending_replacement(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.pending_replacements.remove(&id).is_none() {
                return Err(refused());
            }
            Ok(Response::Empty)
        })
    }
    fn set_created_permissions(
        &mut self,
        directory: u64,
        name: EntryName,
        expected: [u64; 2],
        mode: u32,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.directories
                .get(&directory)
                .ok_or_else(refused)?
                .set_created_permissions(&name, expected, mode)
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn set_temporary_permissions(
        &mut self,
        id: u64,
        mode: u32,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.created_temporaries
                .get(&id)
                .ok_or_else(refused)?
                .set_permissions(mode)
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn retire_created(
        &mut self,
        directory: u64,
        name: EntryName,
        expected: [u64; 2],
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.jobs.len() >= HANDLES || self.next == u64::MAX {
                return Err(crate::native_reply::bounded_refusal());
            }
            let job = self
                .directories
                .get(&directory)
                .ok_or_else(refused)?
                .retire_created(&name, expected)
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.jobs.insert(self.next, job);
            Ok(Response::Handle(self.next))
        })
    }
    fn created_temporary(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.created_temporaries.len() >= 64 || self.next == u64::MAX {
                return Err(crate::native_reply::bounded_refusal());
            }
            let temporary = self
                .jobs
                .get(&id)
                .ok_or_else(refused)?
                .created_temporary()
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.created_temporaries.insert(self.next, temporary);
            Ok(Response::Handle(self.next))
        })
    }
    fn check_created_temporary(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.created_temporaries
                .get(&id)
                .ok_or_else(refused)?
                .check()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn prepare_created_atomic_save(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            use crate::directory_types::AtomicSavePreparation as Outcome;
            if self.pending_replacements.len() >= 64 || self.next == u64::MAX {
                return Ok(Response::AtomicRefused(
                    "Local reservation table full before source request".into(),
                ));
            }
            match self
                .created_temporaries
                .get(&id)
                .ok_or_else(refused)?
                .prepare_atomic_save()
                .await
            {
                Outcome::Prepared(pending) => {
                    self.next = self.next.checked_add(1).ok_or_else(refused)?;
                    self.pending_replacements.insert(self.next, pending);
                    Ok(Response::AtomicPrepared(self.next))
                }
                Outcome::Refused(error) => Ok(Response::AtomicRefused(error)),
                Outcome::Unknown(error) => Ok(Response::AtomicUnknown(error)),
            }
        })
    }
    fn prepare_created_replacement(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.pending_replacements.len() >= 64 || self.next == u64::MAX {
                return Err(crate::native_reply::bounded_refusal());
            }
            let pending = self
                .created_temporaries
                .get(&id)
                .ok_or_else(refused)?
                .prepare_replace()
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.pending_replacements.insert(self.next, pending);
            Ok(Response::Handle(self.next))
        })
    }
    fn retire_created_temporary(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.jobs.len() >= HANDLES || self.next == u64::MAX {
                return Err(crate::native_reply::bounded_refusal());
            }
            let job = self
                .created_temporaries
                .get(&id)
                .ok_or_else(refused)?
                .retire()
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.jobs.insert(self.next, job);
            Ok(Response::Handle(self.next))
        })
    }
    fn release_created_temporary(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.created_temporaries.remove(&id).is_none() {
                return Err(refused());
            }
            Ok(Response::Empty)
        })
    }
    fn replacement_pin(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            // Separate finite custody pool; dropping a job does not drop its pin.
            if self.replacement_pins.len() >= 64 || self.next == u64::MAX {
                return Err(crate::native_reply::bounded_refusal());
            }
            let pin = self
                .jobs
                .get(&id)
                .ok_or_else(refused)?
                .pin_replacement()
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.replacement_pins.insert(self.next, pin);
            Ok(Response::Handle(self.next))
        })
    }
    fn commit_replacement_pin(
        &self,
        id: u64,
        bytes: Vec<u8>,
    ) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.replacement_pins
                .get(&id)
                .ok_or_else(refused)?
                .commit_with_history(&bytes)
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn check_replacement_pin(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.replacement_pins
                .get(&id)
                .ok_or_else(refused)?
                .check()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn observe_replacement_pin(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.replacement_pins
                .get(&id)
                .ok_or_else(refused)?
                .observe()
                .await
                .map(Response::ReplacementObservation)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn release_replacement_pin(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.replacement_pins.remove(&id).is_none() {
                return Err(refused());
            }
            Ok(Response::Empty)
        })
    }
    fn poll(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.jobs
                .get_mut(&id)
                .ok_or_else(refused)?
                .poll()
                .await
                .map(Response::Poll)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn cancel(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.jobs
                .get(&id)
                .ok_or_else(refused)?
                .cancel()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn release_directory(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if id == ROOT_DIRECTORY || self.directories.remove(&id).is_none() {
                return Err(refused());
            }
            Ok(Response::Empty)
        })
    }
    fn release_job(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.jobs.remove(&id).is_none() {
                return Err(refused());
            }
            Ok(Response::Empty)
        })
    }
    fn read(&mut self, id: u64, offset: u64, len: u32) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .read_at(offset, len)
                .await
                .map(Response::Bytes)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn write(&self, id: u64, offset: u64, bytes: Vec<u8>) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .write_at(offset, &bytes)
                .await
                .map(Response::Written)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn append(&self, id: u64, bytes: Vec<u8>) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .append(&bytes)
                .await
                .map(Response::Written)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn flush(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.handles
                .get(&id)
                .ok_or_else(refused)?
                .flush()
                .await
                .map_err(crate::native_reply::client_error)?;
            Ok(Response::Empty)
        })
    }
    fn release(&mut self, id: u64) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.handles.remove(&id).is_none() {
                return Err(refused());
            }
            Ok(Response::Empty)
        })
    }
    // Exhaustive routing keeps every capability operation on the production path.
    fn dispatch(&mut self, request: Request) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            match request {
                request @ (Request::HistoryQuery(..)
                | Request::HistoryReadSubject(..)
                | Request::CheckHistoryRead(..)
                | Request::RetireHistoryReadHandoff(..)
                | Request::ArmHistoryRead(..)
                | Request::ObserveHistoryRead(..)
                | Request::ReleaseHistoryRead(..)
                | Request::CheckPin(..)
                | Request::CheckJob(..)
                | Request::Check(..)
                | Request::OpenCursor(..)
                | Request::CursorPage(..)
                | Request::ReleaseCursor(..)
                | Request::Pin(..)
                | Request::PinStat(..)
                | Request::ReleasePin(..)
                | Request::HandleStat(..)
                | Request::Metadata(..)
                | Request::SyncDirectory(..)
                | Request::Statfs(..)) => self.dispatch_0(request).await,
                request @ (Request::Readlink(..)
                | Request::Stat(..)
                | Request::List(..)
                | Request::Directory(..)
                | Request::Open(..)
                | Request::Create(..)
                | Request::Recoveries(..)
                | Request::RestoreCaptured(..)
                | Request::AtomicSave(..)
                | Request::RenameReplace(..)
                | Request::Rename(..)
                | Request::PrepareReplacement(..)
                | Request::RetirePendingHandoff(..)
                | Request::ArmPendingHandoff(..)
                | Request::AcknowledgedCommittedJob(..)
                | Request::AcknowledgedCommittedPin(..)
                | Request::ObservePendingReplacement(..)
                | Request::CheckPendingReplacement(..)
                | Request::StartPendingReplacement(..)
                | Request::CancelPendingReplacement(..)) => self.dispatch_1(request).await,
                request @ (Request::ReleasePendingReplacement(..)
                | Request::SetCreatedPermissions(..)
                | Request::SetTemporaryPermissions(..)
                | Request::RetireCreated(..)
                | Request::CreatedTemporary(..)
                | Request::CheckCreatedTemporary(..)
                | Request::PrepareCreatedAtomicSave(..)
                | Request::PrepareCreatedReplacement(..)
                | Request::RetireCreatedTemporary(..)
                | Request::ReleaseCreatedTemporary(..)
                | Request::ReplacementPin(..)
                | Request::CommitReplacementPin(..)
                | Request::CheckReplacementPin(..)
                | Request::ObserveReplacementPin(..)
                | Request::ReleaseReplacementPin(..)
                | Request::Poll(..)
                | Request::Cancel(..)
                | Request::ReleaseDirectory(..)
                | Request::ReleaseJob(..)
                | Request::Read(..)) => self.dispatch_2(request).await,
                request @ (Request::Write(..)
                | Request::Append(..)
                | Request::Flush(..)
                | Request::Release(..)) => self.dispatch_3(request).await,
            }
        })
    }
    fn dispatch_0(&mut self, request: Request) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            match request {
                Request::HistoryQuery(id, name, expected, query) => {
                    self.history_query(id, name, expected, query).await
                }
                Request::HistoryReadSubject(id, name) => self.history_read_subject(id, name).await,
                Request::CheckHistoryRead(id, named) => self.check_history_read(id, named).await,
                Request::RetireHistoryReadHandoff(id, ticket) => {
                    self.retire_history_read_handoff(id, ticket).await
                }
                Request::ArmHistoryRead(id, challenge) => {
                    self.arm_history_read(id, challenge).await
                }
                Request::ObserveHistoryRead(id) => self.observe_history_read(id).await,
                Request::ReleaseHistoryRead(id) => self.release_history_read(id).await,
                Request::CheckPin(id) => self.check_pin(id).await,
                Request::CheckJob(id) => self.check_job(id).await,
                Request::Check(id) => self.check(id).await,
                Request::OpenCursor(id) => self.open_cursor(id).await,
                Request::CursorPage(id, sequence) => self.cursor_page(id, sequence).await,
                Request::ReleaseCursor(id) => self.release_cursor(id).await,
                Request::Pin(id, name, expected) => self.pin(id, name, expected).await,
                Request::PinStat(id) => self.pin_stat(id).await,
                Request::ReleasePin(id) => self.release_pin(id).await,
                Request::HandleStat(id) => self.handle_stat(id).await,
                Request::Metadata(id) => self.metadata(id).await,
                Request::SyncDirectory(id) => self.sync_directory(id).await,
                Request::Statfs(id) => self.statfs(id).await,
                _ => Err(refused()),
            }
        })
    }
    fn dispatch_1(&mut self, request: Request) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            match request {
                Request::Readlink(id, name, expected) => self.readlink(id, name, expected).await,
                Request::Stat(id, name) => self.stat(id, name).await,
                Request::List(id, cursor) => self.list(id, cursor).await,
                Request::Directory(id, name) => self.directory(id, name).await,
                Request::Open(id, name, mode, expected) => {
                    self.open(id, name, mode, expected).await
                }
                Request::Create(id, name, folder) => self.create(id, name, folder).await,
                Request::Recoveries(id, after) => self.recoveries(id, after).await,
                Request::RestoreCaptured(id, recovery, name) => {
                    self.restore_captured(id, recovery, name).await
                }
                Request::AtomicSave(id, temporary, expected, target) => {
                    self.atomic_save(id, temporary, expected, target).await
                }
                Request::RenameReplace(id, name, new) => self.rename_replace(id, name, new).await,
                Request::Rename(id, name, new) => self.rename(id, name, new).await,
                Request::PrepareReplacement(id, temporary, expected, victim, old) => {
                    self.prepare_replacement(id, temporary, expected, victim, old)
                        .await
                }
                Request::RetirePendingHandoff(id, ticket) => {
                    self.retire_pending_handoff(id, ticket).await
                }
                Request::ArmPendingHandoff(id, challenge) => {
                    self.arm_pending_handoff(id, challenge).await
                }
                Request::AcknowledgedCommittedJob(id) => self.acknowledged_committed_job(id).await,
                Request::AcknowledgedCommittedPin(id) => self.acknowledged_committed_pin(id).await,
                Request::ObservePendingReplacement(id) => {
                    self.observe_pending_replacement(id).await
                }
                Request::CheckPendingReplacement(id) => self.check_pending_replacement(id).await,
                Request::StartPendingReplacement(id, receipt) => {
                    self.start_pending_replacement(id, receipt).await
                }
                Request::CancelPendingReplacement(id) => self.cancel_pending_replacement(id).await,
                _ => Err(refused()),
            }
        })
    }
    fn dispatch_2(&mut self, request: Request) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            match request {
                Request::ReleasePendingReplacement(id) => {
                    self.release_pending_replacement(id).await
                }
                Request::SetCreatedPermissions(directory, name, expected, mode) => {
                    self.set_created_permissions(directory, name, expected, mode)
                        .await
                }
                Request::SetTemporaryPermissions(id, mode) => {
                    self.set_temporary_permissions(id, mode).await
                }
                Request::RetireCreated(directory, name, expected) => {
                    self.retire_created(directory, name, expected).await
                }
                Request::CreatedTemporary(id) => self.created_temporary(id).await,
                Request::CheckCreatedTemporary(id) => self.check_created_temporary(id).await,
                Request::PrepareCreatedAtomicSave(id) => self.prepare_created_atomic_save(id).await,
                Request::PrepareCreatedReplacement(id) => {
                    self.prepare_created_replacement(id).await
                }
                Request::RetireCreatedTemporary(id) => self.retire_created_temporary(id).await,
                Request::ReleaseCreatedTemporary(id) => self.release_created_temporary(id).await,
                Request::ReplacementPin(id) => self.replacement_pin(id).await,
                Request::CommitReplacementPin(id, bytes) => {
                    self.commit_replacement_pin(id, bytes).await
                }
                Request::CheckReplacementPin(id) => self.check_replacement_pin(id).await,
                Request::ObserveReplacementPin(id) => self.observe_replacement_pin(id).await,
                Request::ReleaseReplacementPin(id) => self.release_replacement_pin(id).await,
                Request::Poll(id) => self.poll(id).await,
                Request::Cancel(id) => self.cancel(id).await,
                Request::ReleaseDirectory(id) => self.release_directory(id).await,
                Request::ReleaseJob(id) => self.release_job(id).await,
                Request::Read(id, offset, len) => self.read(id, offset, len).await,
                _ => Err(refused()),
            }
        })
    }
    fn dispatch_3(&mut self, request: Request) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            match request {
                Request::Write(id, offset, bytes) => self.write(id, offset, bytes).await,
                Request::Append(id, bytes) => self.append(id, bytes).await,
                Request::Flush(id) => self.flush(id).await,
                Request::Release(id) => self.release(id).await,
                _ => Err(refused()),
            }
        })
    }
}

fn run_worker(
    stream: UnixStream,
    receive: tokio::sync::mpsc::Receiver<Call>,
    worker_live: Arc<AtomicBool>,
    worker_fault: Arc<AtomicBool>,
    started: mpsc::SyncSender<io::Result<()>>,
) -> io::Result<()> {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = started.send(Err(error));
            return Err(refused());
        }
    };
    tokio::task::LocalSet::new().block_on(
        &runtime,
        run_rpc(stream, receive, worker_live, worker_fault, started),
    )
}

fn run_rpc(
    stream: UnixStream,
    receive: tokio::sync::mpsc::Receiver<Call>,
    worker_live: Arc<AtomicBool>,
    worker_fault: Arc<AtomicBool>,
    started: mpsc::SyncSender<io::Result<()>>,
) -> LocalFuture<'static, io::Result<()>> {
    Box::pin(async move {
        use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
        stream.set_nonblocking(true)?;
        let stream = tokio::net::UnixStream::from_std(stream)?;
        let (read, write) = stream.into_split();
        let network = capnp_rpc::twoparty::VatNetwork::new(
            read.compat(),
            write.compat_write(),
            capnp_rpc::rpc_twoparty_capnp::Side::Client,
            capnp::message::ReaderOptions::default(),
        );
        let mut rpc = capnp_rpc::RpcSystem::new(Box::new(network), None);
        let raw: crate::WorkspaceFsClient =
            rpc.bootstrap(capnp_rpc::rpc_twoparty_capnp::Side::Server);
        let disconnect = rpc.get_disconnector();
        let mut driver = tokio::task::spawn_local(rpc);
        let client = crate::FsClient::from_raw(raw);
        let outcome = serve(client, receive, &worker_live, &worker_fault, started).await;
        worker_live.store(false, Ordering::Release);
        // The driver is owned here, never detached by bootstrap().
        // Graceful disconnect drains transport; cancellation is observed.
        let disconnect_result = tokio::time::timeout(DEADLINE, disconnect).await;
        let transport = if let Ok(result) = tokio::time::timeout(DEADLINE, &mut driver).await {
            result.map_err(io::Error::other)?.map_err(io::Error::other)
        } else {
            driver.abort();
            let cancelled = driver.await;
            Err(io::Error::other(format!(
                "retained RPC driver did not stop: {cancelled:?}"
            )))
        };
        outcome?;
        disconnect_result
            .map_err(|_| io::Error::other("retained RPC disconnect timed out"))?
            .map_err(io::Error::other)?;
        transport
    })
}

fn serve<'a>(
    client: crate::FsClient,
    mut receive: tokio::sync::mpsc::Receiver<Call>,
    worker_live: &'a AtomicBool,
    worker_fault: &'a AtomicBool,
    started: mpsc::SyncSender<io::Result<()>>,
) -> LocalFuture<'a, io::Result<()>> {
    Box::pin(async move {
        let initial = tokio::time::timeout(DEADLINE, async {
            let directory = client
                .directory()
                .await
                .map_err(crate::native_reply::client_error)?;
            let metadata = directory
                .metadata()
                .await
                .map_err(crate::native_reply::client_error)?;
            if metadata.metadata.kind != crate::EntryKind::Directory {
                return Err(refused());
            }
            Ok(directory)
        })
        .await;
        let root = if let Ok(Ok(directory)) = initial {
            let _ = started.send(Ok(()));
            directory
        } else {
            worker_live.store(false, Ordering::Release);
            let _ = started.send(Err(refused()));
            return Err(refused());
        };
        let mut state = WorkerState {
            directories: HashMap::from([(ROOT_DIRECTORY, root)]),
            handles: HashMap::new(),
            jobs: HashMap::new(),
            replacement_pins: HashMap::new(),
            created_temporaries: HashMap::new(),
            cursors: HashMap::new(),
            pins: HashMap::new(),
            history_reads: HashMap::new(),
            pending_replacements: HashMap::new(),
            next: ROOT_DIRECTORY,
        };
        let mut terminal = None;
        while let Some(call) = receive.recv().await {
            if !worker_live.load(Ordering::Acquire) {
                let _ = call.answer.send(Err(refused()));
                continue;
            }
            let work = state.dispatch(call.request);
            let answer = match tokio::time::timeout(DEADLINE, work).await {
                Ok(Ok(response)) => Ok(response),
                Ok(Err(error)) if crate::native_reply::is_operation_refusal(&error) => Err(error),
                Ok(Err(error)) => {
                    worker_live.store(false, Ordering::Release);
                    terminal = Some(io::Error::new(error.kind(), error.to_string()));
                    Err(error)
                }
                // A timed-out write has an unknown outcome. Never replay it.
                Err(_) => {
                    worker_live.store(false, Ordering::Release);
                    terminal = Some(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "retained source request timed out; outcome unknown",
                    ));
                    Err(refused())
                }
            };
            let _ = call.answer.send(answer);
        }
        match terminal {
            Some(error) => Err(error),
            None if worker_fault.load(Ordering::Acquire) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "retained caller lost request outcome",
            )),
            None => Ok(()),
        }
    })
}
