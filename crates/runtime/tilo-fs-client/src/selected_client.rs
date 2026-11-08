//! Bounded bridge to the existing filesystem capability on a connected fd.
//! The filesystem client stays on its `LocalSet`; FUSE never receives source fds.

use crate::{
    Metadata, OpenMode,
    bridge::{self, Bridge, Dispatch, LIMIT, LocalFuture, refused},
};
use std::{collections::HashMap, io, os::unix::net::UnixStream};

const FILE: &str = crate::SELECTED_FILE;

enum Request {
    Check(u64),
    Stat,
    HandleStat(u64),
    Open(OpenMode),
    Read(u64, u64, u32),
    Write(u64, u64, Vec<u8>),
    Append(u64, Vec<u8>),
    Flush(u64),
    Release(u64),
}
enum Response {
    Metadata(Metadata),
    Handle(u64),
    Bytes(Vec<u8>),
    Written(u32),
    Empty,
}

/// One retained source file's capability on an authenticated stream.
#[derive(Clone)]
pub struct SelectedFileConnection(Bridge<Request, Response>);

impl SelectedFileConnection {
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
        Bridge::connect::<WorkerState>("retained-source-fs", stream).map(Self)
    }
    fn call(&self, request: Request) -> io::Result<Response> {
        self.call_with(|| request)
    }
    fn call_with(&self, request: impl FnOnce() -> Request) -> io::Result<Response> {
        self.0.call_with(request)
    }
    /// # Errors
    /// Refuses revoked source handle scope or transport loss.
    pub fn check_scope(&self, id: u64) -> io::Result<()> {
        match self.call(Request::Check(id))? {
            Response::Empty => Ok(()),
            _ => Err(refused()),
        }
    }
    pub fn metadata(&self) -> io::Result<Metadata> {
        match self.call(Request::Stat)? {
            Response::Metadata(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn metadata_handle(&self, id: u64) -> io::Result<Metadata> {
        match self.call(Request::HandleStat(id))? {
            Response::Metadata(value) => Ok(value),
            _ => Err(refused()),
        }
    }
    pub fn open(&self, mode: OpenMode) -> io::Result<u64> {
        match self.call(Request::Open(mode))? {
            Response::Handle(value) => Ok(value),
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

// Capability clients remain confined to the worker's local task throughout each request.
struct WorkerState {
    handles: HashMap<u64, crate::FileHandle>,
    next: u64,
    client: crate::FsClient,
}
impl WorkerState {
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
    fn stat(&self) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            self.client
                .stat(FILE)
                .await
                .map(Response::Metadata)
                .map_err(crate::native_reply::client_error)
        })
    }
    fn open(&mut self, mode: OpenMode) -> LocalFuture<'_, io::Result<Response>> {
        Box::pin(async move {
            if self.handles.len() >= LIMIT || self.next == u64::MAX {
                return Err(io::Error::other("retained handle limit"));
            }
            let handle = self
                .client
                .open(FILE, mode)
                .await
                .map_err(crate::native_reply::client_error)?;
            self.next = self.next.checked_add(1).ok_or_else(refused)?;
            self.handles.insert(self.next, handle);
            Ok(Response::Handle(self.next))
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
                Request::Check(id) => self.check(id).await,
                Request::HandleStat(id) => self.handle_stat(id).await,
                Request::Stat => self.stat().await,
                Request::Open(mode) => self.open(mode).await,
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
            match client.stat(FILE).await {
                Ok(metadata) if metadata.kind == crate::EntryKind::File => Ok(WorkerState {
                    handles: HashMap::new(),
                    next: 0,
                    client,
                }),
                _ => Err(refused()),
            }
        })
    }
}
