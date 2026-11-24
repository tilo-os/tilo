//! Packets on Windows: one message on a message-mode pipe (`tilo_socket::link`), framed as the
//! count of handles it carries, each handle's value in the receiving process, then the value.
//!
//! A pipe passes no handles itself, so the sender duplicates each into the process at the other
//! end, which it can only do holding that process. So handles go one way only: from this process
//! down to a process it started ([`crate::process_link::spawn`]), whose link holds the started
//! process, and that process takes them off the link it was handed ([`crate::process_link::take`]).
//! A link that names no started process carries no handles either way, and a started process
//! cannot hand its starter any: what it names would be its own handles' values, which mean nothing
//! here, or this process's, which it must not reach.

use std::fmt;
use std::io::{self, Read as _, Write as _};
use std::os::windows::io::{
    AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle as _, OwnedHandle, RawHandle,
};
use std::sync::Arc;

use tilo_socket::Stream;
use windows_sys::Win32::Foundation::{
    DUPLICATE_CLOSE_SOURCE, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::{MAX_FDS, MAX_PACKET, invalid};

/// The bytes before a packet's value: its count of handles.
const COUNT: usize = size_of::<u32>();
/// The bytes of each handle's value.
const VALUE: usize = size_of::<u64>();

/// A connection a packet carries, or a link packets go on, or a buffer a packet hands down (a
/// section, `tilo_shm`'s shared memory here).
pub struct Descriptor {
    held: Held,
    peer: Peer,
}

/// What a descriptor's handle is.
enum Held {
    /// A pipe: a connection or a link.
    Pipe(Stream),
    /// Any other handle: a section.
    Other(OwnedHandle),
}

impl Held {
    fn handle(&self) -> RawHandle {
        match self {
            Self::Pipe(stream) => stream.as_raw_handle(),
            Self::Other(handle) => handle.as_raw_handle(),
        }
    }
}

/// What a caller that wanted a connection was handed instead.
fn no_pipe() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "a handle handed on as a connection is no pipe",
    )
}

/// Which way handles go on a link.
#[derive(Clone)]
enum Peer {
    /// None either way.
    Neither,
    /// Down to the process this one started: the process, which they are duplicated into.
    Started(Arc<OwnedHandle>),
    /// Up from the process that started this one, which duplicated them into this one.
    Starter,
}

impl Descriptor {
    /// A connection that carries no handles.
    #[must_use]
    pub fn new(stream: Stream) -> Self {
        Self {
            held: Held::Pipe(stream),
            peer: Peer::Neither,
        }
    }

    /// `handle`, to hand down in a packet: a pipe is a connection, anything else (a section) is
    /// handed as it is.
    ///
    /// # Errors
    /// The system's error asking which end of a pipe it is.
    pub fn from_handle(handle: OwnedHandle) -> io::Result<Self> {
        Ok(Self {
            held: held(handle)?,
            peer: Peer::Neither,
        })
    }

    /// This process's end of the link to the process `started`, which it started.
    pub(crate) fn to_started(stream: Stream, started: Arc<OwnedHandle>) -> Self {
        Self {
            held: Held::Pipe(stream),
            peer: Peer::Started(started),
        }
    }

    /// The link the process that started this one handed it.
    pub(crate) fn from_starter(stream: Stream) -> Self {
        Self {
            held: Held::Pipe(stream),
            peer: Peer::Starter,
        }
    }

    /// The connection, to read and write as a stream.
    ///
    /// # Errors
    /// `InvalidInput` for a handle that is no pipe (a section).
    pub fn stream(&self) -> io::Result<&Stream> {
        match &self.held {
            Held::Pipe(stream) => Ok(stream),
            Held::Other(_) => Err(no_pipe()),
        }
    }

    /// The connection, owned.
    ///
    /// # Errors
    /// `InvalidInput` for a handle that is no pipe (a section).
    pub fn into_stream(self) -> io::Result<Stream> {
        match self.held {
            Held::Pipe(stream) => Ok(stream),
            Held::Other(_) => Err(no_pipe()),
        }
    }

    /// The handle, owned: a section to map, or a pipe to hand on.
    #[must_use]
    pub fn into_handle(self) -> OwnedHandle {
        match self.held {
            Held::Pipe(stream) => stream.into_handle(),
            Held::Other(handle) => handle,
        }
    }

    /// A second handle on the same connection, carrying handles as this one does.
    ///
    /// # Errors
    /// The handle could not be duplicated.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            held: match &self.held {
                Held::Pipe(stream) => Held::Pipe(stream.try_clone()?),
                Held::Other(handle) => Held::Other(handle.try_clone()?),
            },
            peer: self.peer.clone(),
        })
    }
}

/// `handle` as what it is: a pipe, or any other handle.
fn held(handle: OwnedHandle) -> io::Result<Held> {
    // SAFETY: a live handle, only asked its type.
    let kind =
        unsafe { windows_sys::Win32::Storage::FileSystem::GetFileType(handle.as_raw_handle()) };
    if kind == windows_sys::Win32::Storage::FileSystem::FILE_TYPE_PIPE {
        Stream::from_handle(handle).map(Held::Pipe)
    } else {
        Ok(Held::Other(handle))
    }
}

impl AsRawHandle for Descriptor {
    fn as_raw_handle(&self) -> RawHandle {
        self.held.handle()
    }
}

impl AsHandle for Descriptor {
    fn as_handle(&self) -> BorrowedHandle<'_> {
        // SAFETY: the handle this descriptor owns, borrowed for as long as it lives.
        unsafe { BorrowedHandle::borrow_raw(self.held.handle()) }
    }
}

impl fmt::Debug for Descriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let peer = match self.peer {
            Peer::Neither => "neither",
            Peer::Started(_) => "started",
            Peer::Starter => "starter",
        };
        formatter
            .debug_struct("Descriptor")
            .field("handle", &self.held.handle())
            .field("peer", &peer)
            .finish()
    }
}

/// A connected pair carrying no handles, both ends in this process.
pub(super) fn pair() -> io::Result<(Descriptor, Descriptor)> {
    let (server, client) = tilo_socket::link::pair()?;
    Ok((Descriptor::new(server), Descriptor::new(client)))
}

/// Close the handles `values` duplicated into `process`, which a packet never delivered.
fn close_in(process: &OwnedHandle, values: &[u64]) {
    for value in values {
        // SAFETY: a value this process duplicated into `process` and nothing else owns yet;
        // closing it there needs no target.
        unsafe {
            DuplicateHandle(
                process.as_raw_handle(),
                *value as usize as HANDLE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
                0,
                DUPLICATE_CLOSE_SOURCE,
            );
        }
    }
}

/// Write `encoded` on `socket` with the handles `raw` names, duplicated into its peer.
pub(super) fn send(socket: &Descriptor, encoded: &[u8], raw: &[RawHandle]) -> io::Result<()> {
    let mut values = Vec::with_capacity(raw.len());
    let started = match &socket.peer {
        Peer::Started(process) => Some(process),
        _ if raw.is_empty() => None,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "handles go only down a link to a process this one started",
            ));
        }
    };
    if let Some(process) = started {
        for handle in raw {
            let mut theirs: HANDLE = std::ptr::null_mut();
            // SAFETY: a handle a field of the message holds, borrowed until it is sent, and
            // the started process, which this process holds open.
            let duplicated = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    *handle,
                    process.as_raw_handle(),
                    &raw mut theirs,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if duplicated == 0 {
                let error = io::Error::last_os_error();
                close_in(process, &values);
                return Err(error);
            }
            values.push(theirs as usize as u64);
        }
    }
    let mut frame = Vec::with_capacity(COUNT + VALUE * values.len() + encoded.len());
    frame.extend_from_slice(
        &u32::try_from(values.len())
            .unwrap_or(u32::MAX)
            .to_le_bytes(),
    );
    for value in &values {
        frame.extend_from_slice(&value.to_le_bytes());
    }
    frame.extend_from_slice(encoded);
    let written = (socket.stream()?).write(&frame);
    if !matches!(written, Ok(length) if length == frame.len()) {
        if let Some(process) = started {
            close_in(process, &values);
        }
        return Err(written
            .err()
            .unwrap_or_else(|| io::ErrorKind::WriteZero.into()));
    }
    Ok(())
}

/// The next packet on `socket`: its value's bytes and the handles it carries. `None` once the
/// peer is gone; `WouldBlock` when `wait` is false and nothing waits.
pub(super) fn receive(
    socket: &Descriptor,
    wait: bool,
) -> io::Result<Option<(Vec<u8>, Vec<Descriptor>)>> {
    let stream = socket.stream()?;
    if !wait && !stream.ready()? {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    let mut buffer = vec![0_u8; COUNT + VALUE * MAX_FDS + MAX_PACKET];
    let length = (&*stream).read(&mut buffer)?;
    if length == 0 {
        return Ok(None);
    }
    let frame = buffer.get(..length).unwrap_or_default();
    let (count, rest) = frame
        .split_first_chunk::<COUNT>()
        .ok_or_else(|| invalid("a packet without its count of handles"))?;
    let count = usize::try_from(u32::from_le_bytes(*count)).unwrap_or(usize::MAX);
    if count > MAX_FDS || rest.len() < VALUE * count {
        return Err(invalid("a packet names more handles than it carries"));
    }
    let (values, bytes) = rest.split_at(VALUE * count);
    if count > 0 && !matches!(socket.peer, Peer::Starter) {
        return Err(invalid(
            "handles come only from the process that started this one",
        ));
    }
    // Owned first, every one: a value that is refused below is still closed.
    let owned: Vec<OwnedHandle> = values
        .chunks_exact(VALUE)
        .map(|value| {
            let value = u64::from_le_bytes(value.try_into().unwrap_or_default());
            // SAFETY: the process that started this one duplicated this handle into it for this
            // packet alone, and nothing else here holds its value.
            unsafe { OwnedHandle::from_raw_handle(value as usize as RawHandle) }
        })
        .collect();
    let handles = owned
        .into_iter()
        .map(Descriptor::from_handle)
        .collect::<io::Result<Vec<_>>>()
        .map_err(invalid)?;
    Ok(Some((bytes.to_vec(), handles)))
}
