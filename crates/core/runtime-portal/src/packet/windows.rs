//! Packets on Windows: one message on a message-mode pipe (`tilo_socket::link`), framed as the
//! count of handles it carries, each handle's value, then the value.
//!
//! A pipe passes no handles itself, so only the side that holds the other process moves them
//! (`crate::handles`): it duplicates each handle it sends into that process and sends the value
//! it has there, and it takes each value it receives out of that process. The other side names
//! its own handles, giving up a copy of each for the holder to take, and owns the values the
//! holder sent it. A process holds the processes it started ([`crate::process_link::spawn`]), and
//! the server end of a pipe holds the process that connected to it when that is one of this
//! account's programs. A started process takes handles only from its starter, so a value a peer
//! that holds nothing sends is never read as a handle of this process.

use std::fmt;
use std::io::{self, Read as _, Write as _};
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, OwnedHandle, RawHandle};
use std::sync::Arc;

use tilo_socket::Stream;

use super::{MAX_FDS, MAX_PACKET, invalid};
use crate::handles;

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

/// Who moves the handles on a link.
#[derive(Clone)]
enum Peer {
    /// This process, holding the process at the other end: one it started.
    Held(Arc<handles::Peer>),
    /// The process that started this one, which holds it.
    Starter,
    /// Whoever is at the other end of a pipe handed on: this process holds it when this is the
    /// server end and it is one of this account's programs, asked as a packet carries handles.
    Found,
}

/// How handles move on one packet.
enum Side {
    /// This process duplicates them into and out of the peer.
    Holds(Arc<handles::Peer>),
    /// The peer holds this process: it takes what this one gives up and pushed what it sent.
    Held,
    /// This process gives up its handles to whoever holds it, and takes none: the peer holds
    /// nothing that could have pushed them.
    Gives,
}

impl Descriptor {
    /// A connection, carrying handles as its ends allow (see the module).
    #[must_use]
    pub fn new(stream: Stream) -> Self {
        Self {
            held: Held::Pipe(stream),
            peer: Peer::Found,
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
            peer: Peer::Found,
        })
    }

    /// This process's end of the link to the process `started`, which it started.
    pub(crate) fn to_started(stream: Stream, started: handles::Peer) -> Self {
        Self {
            held: Held::Pipe(stream),
            peer: Peer::Held(Arc::new(started)),
        }
    }

    /// The link the process that started this one handed it.
    pub(crate) fn from_starter(stream: Stream) -> Self {
        Self {
            held: Held::Pipe(stream),
            peer: Peer::Starter,
        }
    }

    /// How handles move on this link now.
    fn side(&self) -> io::Result<Side> {
        match &self.peer {
            Peer::Held(peer) => Ok(Side::Holds(Arc::clone(peer))),
            Peer::Starter => Ok(Side::Held),
            Peer::Found => {
                let stream = self.stream()?;
                if stream.is_server_end() {
                    Ok(Side::Holds(Arc::new(handles::Peer::of(
                        stream.peer_pid()?,
                    )?)))
                } else {
                    Ok(Side::Gives)
                }
            }
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
            Peer::Held(_) => "held",
            Peer::Starter => "starter",
            Peer::Found => "found",
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

/// Write `encoded` on `socket` with the handles `raw` names: pushed into the peer this process
/// holds, or given up for the peer that holds this one.
pub(super) fn send(socket: &Descriptor, encoded: &[u8], raw: &[RawHandle]) -> io::Result<()> {
    let side = if raw.is_empty() {
        Side::Gives
    } else {
        socket.side()?
    };
    let mut values = Vec::with_capacity(raw.len());
    for handle in raw {
        // SAFETY: a handle a field of the message holds, borrowed until it is sent.
        let handle = unsafe { BorrowedHandle::borrow_raw(*handle) };
        let moved = match &side {
            Side::Holds(peer) => peer.push(handle),
            Side::Held | Side::Gives => handles::release(handle),
        };
        match moved {
            Ok(value) => values.push(value),
            Err(error) => {
                take_back(&side, &values);
                return Err(error);
            }
        }
    }
    let mut frame = Vec::with_capacity(
        COUNT
            .saturating_add(VALUE.saturating_mul(values.len()))
            .saturating_add(encoded.len()),
    );
    let count = u32::try_from(values.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many handles"))?;
    frame.extend_from_slice(&count.to_le_bytes());
    for value in &values {
        frame.extend_from_slice(&value.to_le_bytes());
    }
    frame.extend_from_slice(encoded);
    let written = (socket.stream()?).write(&frame);
    if !matches!(written, Ok(length) if length == frame.len()) {
        take_back(&side, &values);
        return Err(written
            .err()
            .unwrap_or_else(|| io::ErrorKind::WriteZero.into()));
    }
    Ok(())
}

/// Close the handles `values` names that a packet never delivered.
fn take_back(side: &Side, values: &[u64]) {
    match side {
        Side::Holds(peer) => peer.close(values),
        Side::Held | Side::Gives => handles::unrelease(values),
    }
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
    if count > MAX_FDS || rest.len() < VALUE.saturating_mul(count) {
        return Err(invalid("a packet names more handles than it carries"));
    }
    let (values, bytes) = rest.split_at(VALUE.saturating_mul(count));
    let values: Vec<u64> = values
        .chunks_exact(VALUE)
        .map(|value| u64::from_le_bytes(value.try_into().unwrap_or_default()))
        .collect();
    if values.is_empty() {
        return Ok(Some((bytes.to_vec(), Vec::new())));
    }
    // Owned first, every one: a value that is refused below is still closed.
    let owned: Vec<OwnedHandle> = match socket.side()? {
        Side::Holds(peer) => {
            let mut owned = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                match peer.pull(*value) {
                    Ok(handle) => owned.push(handle),
                    Err(error) => {
                        peer.close(values.get(index.saturating_add(1)..).unwrap_or_default());
                        return Err(invalid(error));
                    }
                }
            }
            owned
        }
        // SAFETY: the process that started this one duplicated each of these into it for this
        // packet alone, and nothing else here holds their values.
        Side::Held => values
            .iter()
            .filter_map(|value| unsafe { handles::pushed(*value) })
            .collect(),
        Side::Gives => {
            return Err(invalid(
                "handles come only from a process that holds this one",
            ));
        }
    };
    if owned.len() != values.len() {
        return Err(invalid("a packet names a handle that is none"));
    }
    let handles = owned
        .into_iter()
        .map(Descriptor::from_handle)
        .collect::<io::Result<Vec<_>>>()
        .map_err(invalid)?;
    Ok(Some((bytes.to_vec(), handles)))
}
