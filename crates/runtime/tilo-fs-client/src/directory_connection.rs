//! Bounded bridge to the existing filesystem capability on a connected fd.
//! The filesystem client stays on its `LocalSet`; FUSE never receives source fds.

use crate::{
    Metadata, OpenMode,
    bridge::{self, Bridge, Dispatch, LocalFuture, refused},
};
use std::{collections::HashMap, io, os::unix::net::UnixStream};

const HANDLES: usize = 256;
const METADATA_PINS: usize = 4096;
pub const ROOT_DIRECTORY: u64 = 1;
use crate::{
    EntryName,
    directory_client::{
        DirectoryClient, DirectoryCursorClient, MetadataPinClient, NativeJobClient,
    },
    directory_types::{DirectoryRows, DirectoryStat, DirectoryStreamPage, JobPoll},
};

enum Request {
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
    Rename(u64, EntryName, EntryName),
    Recoveries(u64, Vec<u8>),
    RestoreCaptured(u64, [u8; 16], EntryName),
    Poll(u64),
    Cancel(u64),
    ReleaseDirectory(u64),
    ReleaseJob(u64),
    HandleStat(u64),
    Read(u64, u64, u32),
    Write(u64, u64, Vec<u8>),
    Append(u64, Vec<u8>),
    Flush(u64),
    Release(u64),
}
enum Response {
    CursorPage(DirectoryStreamPage),
    Metadata(Metadata),
    Stat(Option<DirectoryStat>),
    Statfs(crate::directory_types::DirectoryFilesystemStats),
    DirectoryStat(DirectoryStat),
    Rows(DirectoryRows),
    Recoveries(crate::directory_types::RecoveryPage),
    Poll(JobPoll),
    Handle(u64),
    Bytes(Vec<u8>),
    Written(u32),
    Empty,
}

/// One retained directory tree's capability on an authenticated stream.
#[derive(Clone)]
pub struct DirectoryConnection(Bridge<Request, Response>);

impl DirectoryConnection {
    /// Close the queue and observe the exact RPC worker and transport outcome.
    /// Requires sole custody after all FUSE callbacks and handles have retired.
    pub fn retire(self) -> io::Result<()> {
        self.0.retire()
    }
    /// Terminally close this exact authenticated channel before waiting for
    /// callbacks. The source provider observes peer death at native effect gates.
    pub fn revoke(&self) {
        self.0.revoke();
    }
    pub fn connected(stream: UnixStream) -> io::Result<Self> {
        Bridge::connect::<WorkerState>("retained-directory-fs", stream).map(Self)
    }
    fn call(&self, request: Request) -> io::Result<Response> {
        self.call_with(|| request)
    }
    fn call_with(&self, request: impl FnOnce() -> Request) -> io::Result<Response> {
        self.0.call_with(request)
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
    pub fn rename_no_replace(
        &self,
        directory: u64,
        name: &EntryName,
        new: &EntryName,
    ) -> io::Result<u64> {
        self.handle(Request::Rename(directory, name.clone(), new.clone()))
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

// Capability clients remain confined to the worker's local task throughout each request.
struct WorkerState {
    directories: HashMap<u64, DirectoryClient>,
    handles: HashMap<u64, crate::FileHandle>,
    jobs: HashMap<u64, NativeJobClient>,
    cursors: HashMap<u64, DirectoryCursorClient>,
    pins: HashMap<u64, MetadataPinClient>,
    next: u64,
}
impl WorkerState {
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
    fn dispatch(&mut self, request: Request) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            match request {
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
                Request::Rename(id, name, new) => self.rename(id, name, new).await,
                Request::Poll(id) => self.poll(id).await,
                Request::Cancel(id) => self.cancel(id).await,
                Request::ReleaseDirectory(id) => self.release_directory(id).await,
                Request::ReleaseJob(id) => self.release_job(id).await,
                Request::Read(id, offset, len) => self.read(id, offset, len).await,
                Request::Write(id, offset, bytes) => self.write(id, offset, bytes).await,
                Request::Append(id, bytes) => self.append(id, bytes).await,
                Request::Flush(id) => self.flush(id).await,
                Request::Release(id) => self.release(id).await,
            }
        })
    }
}

impl Dispatch<Request, Response> for WorkerState {
    fn dispatch(&mut self, request: Request) -> LocalFuture<'_, io::Result<Response>> {
        WorkerState::dispatch(self, request)
    }
}

impl bridge::Serve<Request, Response> for WorkerState {
    fn admit(client: crate::FsClient) -> LocalFuture<'static, io::Result<Self>> {
        Box::pin(async move {
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
            Ok(WorkerState {
                directories: HashMap::from([(ROOT_DIRECTORY, directory)]),
                handles: HashMap::new(),
                jobs: HashMap::new(),
                cursors: HashMap::new(),
                pins: HashMap::new(),
                next: ROOT_DIRECTORY,
            })
        })
    }
}
