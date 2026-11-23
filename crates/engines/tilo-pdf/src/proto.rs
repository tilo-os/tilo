//! The client ↔ helper protocol.
//!
//! Messages are postcard frames behind a u32-LE length prefix ([`FRAMING`]).
//! Descriptors travel as separate one-byte `SCM_RIGHTS` messages
//! ([`runtime_portal::fd_transport`]) at fixed points: after an `Open` request
//! (the document), after a `Render` that attaches a new pixel buffer, and
//! after an `Exported` response (the PDF, in a sealed memory file).
//!
//! Pixel buffers are memory files the client owns and reuses: the helper maps
//! each one once and renders straight into it, so a page render moves no pixel
//! through the socket. A buffer is writable by the helper, so a compromised
//! helper can alter pixels the client displays, never anything the client
//! interprets. Frames never pass through a buffer, so a descriptor message is
//! always the next thing on the stream when it is expected.

#[cfg(unix)]
use std::io;
#[cfg(unix)]
use std::os::fd::{BorrowedFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;

use serde::{Deserialize, Serialize};
#[cfg(unix)]
use wire_frame::{FrameError, Framing};

#[cfg(unix)]
use crate::edits::Edits;
#[cfg(unix)]
use crate::model::{AnnotationInfo, DocumentInfo, FormField, Link, OutlineEntry, RenderOptions};
#[cfg(unix)]
use crate::text::RawChar;

/// Framing of every message between client and helper. Pixels and exported
/// files travel in shared memory, never in a frame.
#[cfg(unix)]
pub const FRAMING: Framing = Framing::little_endian(16 * 1024 * 1024);

/// Bumped on any incompatible message change; the helper refuses a client
/// speaking another version.
#[cfg(unix)]
pub const PROTOCOL_VERSION: u32 = 3;

#[cfg(unix)]
const FD_MARKER: &[u8] = b"F";

/// What an `Open` request's descriptor contains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpenKind {
    Pdf,
    Image,
}

#[cfg(unix)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    /// Followed by one descriptor holding the bytes: its first `len` bytes
    /// (a memory file can be larger than its content), or all of it.
    Open {
        doc: u64,
        len: Option<u64>,
        kind: OpenKind,
        password: Option<String>,
    },
    Close {
        doc: u64,
    },
    /// Render into pixel buffer `buffer`. When `attach` is set the buffer is
    /// new to this helper and its descriptor follows the request.
    Render {
        doc: u64,
        page: u32,
        options: RenderOptions,
        buffer: u64,
        buffer_len: u64,
        attach: bool,
    },
    ReleaseBuffer {
        buffer: u64,
    },
    Text {
        doc: u64,
        page: u32,
    },
    Links {
        doc: u64,
        page: u32,
    },
    Annotations {
        doc: u64,
        page: u32,
    },
    FormFields {
        doc: u64,
    },
    Outline {
        doc: u64,
    },
    Export {
        doc: u64,
        edits: Edits,
        imports: Vec<u64>,
    },
    /// Ask the helper to attempt things its sandbox must refuse.
    Probe {
        path: String,
    },
    /// Make a development build of the helper abort while handling this
    /// request, to exercise crash recovery. Release builds refuse it.
    Abort,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureKind {
    Format,
    Password,
    Invalid,
    Limit,
    Engine,
    Sandbox,
}

/// What the helper's sandbox let a probe do.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent verdict per probe"
)]
pub struct ProbeReport {
    /// `open(path)` for reading succeeded.
    pub opened_file: bool,
    pub created_file: bool,
    pub inet_socket: bool,
    /// A Unix socket could be bound next to `path`.
    pub unix_socket: bool,
    /// `execve` of `/bin/true` returned (it must fail).
    pub exec: bool,
}

#[cfg(unix)]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Response {
    /// Sent once after start-up, once the sandbox is in force.
    Ready {
        version: u32,
    },
    Opened(DocumentInfo),
    /// The request's pixel buffer now starts with `height` rows of `width`
    /// RGBA pixels, tightly packed.
    Rendered {
        width: u32,
        height: u32,
    },
    Text(Vec<RawChar>),
    Links(Vec<Link>),
    Annotations(Vec<AnnotationInfo>),
    FormFields(Vec<FormField>),
    Outline(Vec<OutlineEntry>),
    /// Followed by the sealed memfd holding `len` bytes of PDF.
    Exported {
        len: u64,
    },
    Closed,
    Probe(ProbeReport),
    Failed {
        kind: FailureKind,
        message: String,
    },
}

#[cfg(unix)]
pub struct Channel {
    stream: UnixStream,
}

#[cfg(unix)]
impl Channel {
    pub fn new(stream: UnixStream) -> Self {
        Self { stream }
    }

    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }

    /// # Errors
    ///
    /// Encoding or socket failures.
    pub fn send<T: Serialize>(&mut self, message: &T) -> io::Result<()> {
        Ok(FRAMING.write_value(&mut self.stream, message)?)
    }

    /// # Errors
    ///
    /// Decoding or socket failures, including end of stream.
    pub fn recv<T: for<'de> Deserialize<'de>>(&mut self) -> io::Result<T> {
        Ok(FRAMING
            .read_value(&mut self.stream)?
            .ok_or(FrameError::Closed)?)
    }

    /// # Errors
    ///
    /// Socket failures.
    pub fn send_fd(&self, fd: BorrowedFd<'_>) -> io::Result<()> {
        runtime_portal::fd_transport::send_with_fd(&self.stream, FD_MARKER, fd)
    }

    pub fn recv_fd(&self) -> io::Result<OwnedFd> {
        let (payload, fd) = runtime_portal::fd_transport::recv_with_fd(&self.stream)?;
        match fd {
            Some(fd) if payload == FD_MARKER => Ok(fd),
            _ if payload.is_empty() => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "PDF helper closed the connection",
            )),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected a descriptor from the PDF helper",
            )),
        }
    }
}
