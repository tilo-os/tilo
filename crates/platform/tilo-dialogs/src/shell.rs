//! Choosers the session shell draws itself, for sessions whose apps reach no
//! desktop portal (a provider's headless guest).
//!
//! The launcher hands every app a connection to the shell's chooser service
//! ([`FD_ENV`]) and names the app and its workspace on it before the sandbox
//! starts, so the shell knows who asks without trusting the app. Requests and
//! replies are newline-delimited JSON ([`Request`], [`Reply`]). The shell
//! shows its own file chooser. Legacy path results remain limited to existing
//! grants; owner-confirmed contents selections expose only bounded bytes on this
//! connection, never a directory grant or a replayable host path. When nobody is at the
//! computer it answers [`Reply::Nobody`] and the app falls back to the chooser it
//! brought ([`crate::unattended_picker`] or the desktop's own), so a session with no
//! portal behind it still answers a Save.
//!
//! Links go the same way ([`ShellLink::opener`]): the shell opens a link in
//! the app that claims its scheme, as its own search does, and answers
//! [`Reply::Unclaimed`] for one no app it knows opens, which the app then
//! hands to its desktop's opener.

#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "the descriptor transport reads and writes the system's control buffers by position"
)]
#![allow(
    clippy::borrow_as_ptr,
    reason = "the descriptor transport hands the system's calls the message it filled"
)]

use std::collections::{HashMap, VecDeque};
use std::fs::File;
#[cfg(unix)]
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tilo_socket::Stream;

use serde::{Deserialize, Serialize};

use crate::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, LaunchChannel, LaunchOutcome, LaunchRequest,
    MultiChannel, MultiOutcome, OpenFileChannel, OpenFileOutcome, OpenedFile, PickKind,
    PickRequest, PickedContents, PortalChannel, PortalOutcome, SaveChannel, SaveOutcome,
};
use crate::{FilePicker, answered};

pub const SERVICE: &str = "chooser";
pub const FD_ENV: &str = "TILO_SVC_CHOOSER_FD";
pub const PROTOCOL: u16 = 1;
/// The largest frame either end writes: a folder of many files answers with one grant each, so
/// a choice of ten thousand small files fits on one line.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

const NOBODY_TO_CHOOSE_APP: &str = "nobody is at this computer to choose an app";
const OUT_OF_TURN: &str = "the session's shell answered out of turn";
// What an app may show the person when the shell's chooser itself is out
// of reach: what happened, and what to do (`PortalOutcome::Failed` carries a
// sentence for the person, never a transport error).
const CLOSED: &str = "The file chooser closed before you chose. Try again.";
const UNREACHABLE: &str = "The file chooser didn't open. Try again.";
const CHOOSER_OUT_OF_TURN: &str = "The file chooser gave an answer that doesn't fit. Try again.";
const LINK_ANSWER: std::time::Duration = std::time::Duration::from_secs(30);
/// How long a finished save waits for the chooser to rename it over the file.
const SAVE_COMMIT_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handshake {
    pub protocol: u16,
    pub app_id: String,
    pub workspace_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PickMode {
    Open,
    Directory,
    /// A destination directory; older services reject this unknown mode rather
    /// than silently answering a read-only request.
    WritableDirectory,
    Save,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filter {
    pub name: String,
    #[serde(default)]
    pub globs: Vec<String>,
    #[serde(default)]
    pub mimetypes: Vec<String>,
}

/// A private, read-only selection on one authenticated chooser connection.
/// The token names an already-open regular file, never a replayable host path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePortalGrant {
    pub request: u64,
    pub token: String,
    pub name: String,
    pub size: u64,
}

pub const MAX_CONTENT_BYTES: u64 = 64 * 1024 * 1024;
pub const CONTENT_CHUNK_BYTES: u32 = 8192;
/// The most files one contents pick hands over: a folder a person chooses may hold a shelf of
/// photographs, and the byte bound still keeps what is read bounded.
pub const MAX_CONTENT_FILES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Pick {
        id: u64,
        mode: PickMode,
        #[serde(default)]
        multiple: bool,
        title: String,
        #[serde(default)]
        accept_label: String,
        #[serde(default)]
        suggested_name: Option<String>,
        #[serde(default)]
        filters: Vec<Filter>,
    },
    /// Turn a reference the chooser answered back into an open file: a window restored on the
    /// file it showed, or a file inside a folder the person chose. `write` asks for a Save's
    /// file beside it, opened for writing.
    Reopen {
        id: u64,
        reference: PathBuf,
        #[serde(default)]
        write: bool,
    },
    /// A Save's bytes are written through the descriptor: rename the file beside the chosen one
    /// over it.
    Saved {
        id: u64,
        reference: PathBuf,
    },
    PickContents {
        id: u64,
        multiple: bool,
        /// Whether the person chooses a folder, whose files the service walks and hands over
        /// with their paths inside it.
        #[serde(default)]
        directory: bool,
        title: String,
        accept_label: String,
        filters: Vec<Filter>,
    },
    ReadContents {
        id: u64,
        grant: FilePortalGrant,
        offset: u64,
        max_bytes: u32,
    },
    ReleaseContents {
        id: u64,
        grant: FilePortalGrant,
    },
    Open {
        id: u64,
        path: PathBuf,
    },
    OpenUri {
        id: u64,
        uri: String,
    },
    ChooseApp {
        id: u64,
        purpose: String,
    },
    AwaitIdentity {
        id: u64,
    },
    ChooseIdentity {
        id: u64,
    },
}

/// One chosen file or folder: what reopens it later, and what it is called.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandedDocument {
    /// The documents-store path of the chosen item, which the chooser turns back into a
    /// descriptor ([`Request::Reopen`]); never a host path the app could use itself.
    pub reference: PathBuf,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChosenApp {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub window_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    ContentsGranted {
        id: u64,
        grants: Vec<FilePortalGrant>,
    },
    ContentsChunk {
        id: u64,
        bytes: Vec<u8>,
        eof: bool,
    },
    ContentsReleased {
        id: u64,
    },
    /// The chosen files or folders, each with the descriptor that opens it: the descriptors
    /// travel with this frame as `SCM_RIGHTS`, one per document, in order.
    Handed {
        id: u64,
        documents: Vec<HandedDocument>,
    },
    /// A Save's bytes are in the file beside the chosen one, which is now renamed over it.
    Saved {
        id: u64,
    },
    /// Retired: an answer from before the chooser handed descriptors. Read as a choice of
    /// nothing; never written.
    Picked {
        id: u64,
        paths: Vec<PathBuf>,
    },
    Cancelled {
        id: u64,
    },
    Nobody {
        id: u64,
    },
    Opened {
        id: u64,
    },
    Unclaimed {
        id: u64,
    },
    Failed {
        id: u64,
        reason: String,
    },
    AppChosen {
        id: u64,
        app: ChosenApp,
    },
    IdentitySelected {
        id: u64,
    },
}

impl Reply {
    #[must_use]
    pub fn id(&self) -> u64 {
        match self {
            Reply::ContentsGranted { id, .. }
            | Reply::ContentsChunk { id, .. }
            | Reply::ContentsReleased { id }
            | Reply::Handed { id, .. }
            | Reply::Saved { id }
            | Reply::Picked { id, .. }
            | Reply::Cancelled { id }
            | Reply::Nobody { id }
            | Reply::Opened { id }
            | Reply::Unclaimed { id }
            | Reply::IdentitySelected { id }
            | Reply::AppChosen { id, .. }
            | Reply::Failed { id, .. } => *id,
        }
    }
}

impl Filter {
    fn from_portal(filter: &FileFilter) -> Self {
        Self {
            name: filter.name.clone(),
            globs: filter.globs.clone(),
            mimetypes: filter.mimetypes.clone(),
        }
    }
}

/// The most descriptors one message carries: the kernel's own bound leaves room for fewer, so a
/// choice of ten thousand files arrives in several messages.
const MAX_FDS_PER_MESSAGE: usize = 64;

/// A descriptor handed with a reply: an open file or folder, whatever the app's sandbox did not
/// let it reach by path. Only a Unix host hands one; elsewhere a choice is answered by its
/// reference alone.
#[cfg(unix)]
pub(crate) type Descriptor = OwnedFd;
#[cfg(not(unix))]
#[derive(Debug)]
pub(crate) struct Descriptor;

type Answer = Box<dyn FnOnce(Reply, Vec<Descriptor>) + Send>;

struct Connection {
    writer: Mutex<Stream>,
    pending: Mutex<Option<HashMap<u64, Answer>>>,
    next_id: AtomicU64,
}

impl Connection {
    /// Send `request` (its id is filled in) and call `answer` with the reply and the descriptors
    /// that came with it, on the connection's reader thread, or with a failure if it cannot be
    /// sent.
    fn ask(&self, build: impl FnOnce(u64) -> Request, answer: Answer) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let refused = match self.pending.lock() {
            Ok(mut pending) => match pending.as_mut() {
                Some(pending) => {
                    pending.insert(id, answer);
                    None
                }
                None => Some(answer),
            },
            Err(_) => Some(answer),
        };
        if let Some(answer) = refused {
            answer(
                Reply::Failed {
                    id,
                    reason: CLOSED.to_owned(),
                },
                Vec::new(),
            );
            return;
        }
        let sent = serde_json::to_vec(&build(id))
            .map_err(std::io::Error::other)
            .and_then(|mut frame| {
                frame.push(b'\n');
                let mut writer = self
                    .writer
                    .lock()
                    .map_err(|_| std::io::Error::other("chooser writer poisoned"))?;
                let mut written = 0;
                while written < frame.len() {
                    written += std::io::Write::write(&mut *writer, &frame[written..])?;
                }
                std::io::Write::flush(&mut *writer)
            });
        if let Err(error) = sent
            && let Some(answer) = self.take(id)
        {
            tracing::warn!(%error, "the session's file chooser is out of reach");
            answer(
                Reply::Failed {
                    id,
                    reason: UNREACHABLE.to_owned(),
                },
                Vec::new(),
            );
        }
    }

    fn take(&self, id: u64) -> Option<Answer> {
        self.pending
            .lock()
            .ok()?
            .as_mut()
            .and_then(|pending| pending.remove(&id))
    }

    /// The shell hung up: answer everything still waiting, and refuse what comes.
    fn close(&self) {
        let waiting = self
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.take())
            .unwrap_or_default();
        for (id, answer) in waiting {
            answer(
                Reply::Failed {
                    id,
                    reason: CLOSED.to_owned(),
                },
                Vec::new(),
            );
        }
    }
}

fn read_replies(connection: &Connection, stream: Stream) {
    let mut reader = FrameReader::new(stream);
    while let Ok(Some((reply, fds))) = reader.reply() {
        if let Some(answer) = connection.take(reply.id()) {
            answer(reply, fds);
        }
    }
    connection.close();
}

/// Newline-delimited frames, with the descriptors a reply carried: a stream socket delivers a
/// frame's bytes and its descriptors together, so a reader keeps the descriptors in the order
/// they arrived and hands the next one to each chosen document.
pub(crate) struct FrameReader {
    stream: Stream,
    buffer: Vec<u8>,
    #[cfg(unix)]
    fds: VecDeque<Descriptor>,
}

impl FrameReader {
    pub(crate) fn new(stream: Stream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            #[cfg(unix)]
            fds: VecDeque::new(),
        }
    }

    /// The next frame's line (without its newline), or `None` when the far end hung up.
    pub(crate) fn line(&mut self) -> std::io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(at) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let line = self.buffer[..at].to_vec();
                self.buffer.drain(..=at);
                return Ok(Some(line));
            }
            if self.buffer.len() > MAX_FRAME_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "a chooser frame is larger than the wire allows",
                ));
            }
            if !self.fill()? {
                return Ok(None);
            }
        }
    }

    /// The next reply, with the descriptors its documents carried.
    pub(crate) fn reply(&mut self) -> std::io::Result<Option<(Reply, Vec<Descriptor>)>> {
        let Some(line) = self.line()? else {
            return Ok(None);
        };
        let reply: Reply = match serde_json::from_slice(line.trim_ascii_end()) {
            Ok(reply) => reply,
            Err(error) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("an unreadable chooser reply: {error}"),
                ));
            }
        };
        let wanted = match &reply {
            Reply::Handed { documents, .. } => documents.len(),
            _ => 0,
        };
        let fds = self.take_fds(wanted);
        Ok(Some((reply, fds)))
    }

    #[cfg(unix)]
    fn take_fds(&mut self, wanted: usize) -> Vec<Descriptor> {
        (0..wanted).filter_map(|_| self.fds.pop_front()).collect()
    }

    #[cfg(not(unix))]
    fn take_fds(&mut self, _wanted: usize) -> Vec<Descriptor> {
        Vec::new()
    }

    #[cfg(not(unix))]
    fn fill(&mut self) -> std::io::Result<bool> {
        let mut bytes = [0_u8; 8192];
        let read = std::io::Read::read(&mut self.stream, &mut bytes)?;
        if read == 0 {
            return Ok(false);
        }
        self.buffer.extend_from_slice(&bytes[..read]);
        Ok(true)
    }

    #[cfg(unix)]
    fn fill(&mut self) -> std::io::Result<bool> {
        let mut bytes = [0_u8; 8192];
        // Room for the descriptors one message may carry, aligned for the control message.
        let space = cmsg_space(MAX_FDS_PER_MESSAGE * std::mem::size_of::<RawFd>());
        let mut control = vec![0_u8; space];
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        // SAFETY: an empty message header filled in below, with an iovec and control buffer
        // sized for what the system writes into them.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &raw mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len();
        // SAFETY: `recvmsg` reads the descriptor and writes only the buffers above.
        let read = unsafe { libc::recvmsg(self.stream.as_raw_fd(), &mut message, 0) };
        if read < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if read == 0 {
            return Ok(false);
        }
        self.buffer
            .extend_from_slice(&bytes[..usize::try_from(read).unwrap_or(0)]);
        if message.msg_flags & libc::MSG_CTRUNC != 0 {
            tracing::warn!("the chooser's descriptors were truncated");
        }
        // SAFETY: the header is the one `recvmsg` filled, so its control messages are valid.
        let mut header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        while !header.is_null() {
            // SAFETY: `header` points at a control message the system wrote.
            let control = unsafe { &*header };
            if control.cmsg_level == libc::SOL_SOCKET && control.cmsg_type == libc::SCM_RIGHTS {
                let data = unsafe { libc::CMSG_DATA(header) }.cast::<RawFd>();
                let count = control.cmsg_len as usize - unsafe { libc::CMSG_LEN(0) } as usize;
                for at in 0..count / std::mem::size_of::<RawFd>() {
                    // SAFETY: the control message holds `count` bytes of descriptors.
                    let fd = unsafe { *data.add(at) };
                    // SAFETY: the descriptor arrived with this message and nothing else owns it.
                    self.fds.push_back(unsafe { OwnedFd::from_raw_fd(fd) });
                }
            }
            // SAFETY: `header` is one of the message's control messages.
            header = unsafe { libc::CMSG_NXTHDR(&message, header) };
        }
        Ok(true)
    }
}

/// The file a handed descriptor opens.
#[cfg(unix)]
#[allow(
    clippy::unnecessary_wraps,
    reason = "a host that cannot hand a descriptor answers the same call with an error"
)]
fn handed_file(fd: Descriptor) -> std::io::Result<File> {
    Ok(File::from(fd))
}

#[cfg(not(unix))]
#[allow(
    clippy::unnecessary_wraps,
    reason = "a host that cannot hand a descriptor answers the same call with an error"
)]
fn handed_file(_fd: Descriptor) -> std::io::Result<File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this host cannot hand a file",
    ))
}

#[cfg(unix)]
fn cmsg_space(bytes: usize) -> usize {
    // SAFETY: `CMSG_SPACE` is a size computation.
    unsafe { libc::CMSG_SPACE(u32::try_from(bytes).unwrap_or(u32::MAX)) as usize }
}

/// Write `line` as one frame, carrying `fds` as `SCM_RIGHTS`: the descriptors travel in the same
/// message as the first bytes of the line, so the reader sees them in the order the line names
/// the documents they open.
pub(crate) fn send_frame(stream: &Stream, line: &[u8], fds: &[OwnedFd]) -> std::io::Result<()> {
    let pieces = fds.len().div_ceil(MAX_FDS_PER_MESSAGE).max(1);
    let size = line.len().div_ceil(pieces).max(1);
    let mut at = 0;
    for (index, piece) in line.chunks(size).enumerate() {
        let from = index.saturating_mul(MAX_FDS_PER_MESSAGE).min(fds.len());
        let to = from.saturating_add(MAX_FDS_PER_MESSAGE).min(fds.len());
        write_piece(stream, piece, &fds[from..to])?;
        at += piece.len();
    }
    if at < line.len() {
        write_piece(stream, &line[at..], &[])?;
    }
    Ok(())
}

#[cfg(unix)]
fn write_piece(stream: &Stream, bytes: &[u8], fds: &[Descriptor]) -> std::io::Result<()> {
    let mut iov = libc::iovec {
        iov_base: bytes.as_ptr().cast_mut().cast(),
        iov_len: bytes.len(),
    };
    // SAFETY: an empty message header filled in below.
    let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
    message.msg_iov = &raw mut iov;
    message.msg_iovlen = 1;
    let mut control = if fds.is_empty() {
        Vec::new()
    } else {
        vec![0_u8; cmsg_space(fds.len() * std::mem::size_of::<RawFd>())]
    };
    if !fds.is_empty() {
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = control.len();
        // SAFETY: the header is empty and the control buffer has room for one message.
        let header = unsafe { libc::CMSG_FIRSTHDR(&message) };
        if header.is_null() {
            return Err(std::io::Error::other(
                "no room for the chooser's descriptors",
            ));
        }
        // SAFETY: `header` points into `control`, sized by `cmsg_space`.
        unsafe {
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len = libc::CMSG_LEN(
                u32::try_from(fds.len() * std::mem::size_of::<RawFd>()).unwrap_or(u32::MAX),
            ) as usize;
            let data = libc::CMSG_DATA(header).cast::<RawFd>();
            for (at, fd) in fds.iter().enumerate() {
                *data.add(at) = fd.as_raw_fd();
            }
        }
    }
    // SAFETY: `sendmsg` reads the descriptor and the buffers it names.
    let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &message, libc::MSG_NOSIGNAL) };
    if sent < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let sent = usize::try_from(sent).unwrap_or(0);
    if sent < bytes.len() {
        let mut rest = &bytes[sent..];
        while !rest.is_empty() {
            match std::io::Write::write(&mut &*stream, rest) {
                Ok(0) => {
                    return Err(std::io::Error::from(std::io::ErrorKind::WriteZero));
                }
                Ok(written) => rest = &rest[written..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

#[derive(Clone)]
pub struct ShellLink(Arc<LinkInner>);

struct LinkInner {
    connection: Arc<Connection>,
}

impl Drop for LinkInner {
    fn drop(&mut self) {
        if let Ok(writer) = self.connection.writer.lock() {
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
        self.connection.close();
    }
}

impl ShellLink {
    pub fn connect(stream: Stream) -> std::io::Result<Self> {
        let reader = stream.try_clone()?;
        let connection = Arc::new(Connection {
            writer: Mutex::new(stream),
            pending: Mutex::new(Some(HashMap::new())),
            next_id: AtomicU64::new(1),
        });
        let reading = Arc::clone(&connection);
        std::thread::Builder::new()
            .name("tilo-shell-chooser".into())
            .spawn(move || read_replies(&reading, reader))?;
        Ok(Self(Arc::new(LinkInner { connection })))
    }

    #[must_use]
    pub fn picker(&self, elsewhere: Arc<dyn FilePicker>) -> Arc<dyn FilePicker> {
        Arc::new(ShellChooser {
            link: self.clone(),
            elsewhere,
        })
    }

    #[must_use]
    pub fn opener(&self, elsewhere: Arc<dyn crate::UriOpener>) -> Arc<dyn crate::UriOpener> {
        Arc::new(ShellOpener {
            link: self.clone(),
            elsewhere,
        })
    }

    pub fn choose_app(
        &self,
        purpose: &str,
        chosen: impl FnOnce(Result<Option<ChosenApp>, String>) + Send + 'static,
    ) {
        let purpose = purpose.to_owned();
        self.connection().ask(
            move |id| Request::ChooseApp { id, purpose },
            Box::new(move |reply, _fds| {
                chosen(match reply {
                    Reply::AppChosen { app, .. } => Ok(Some(app)),
                    Reply::Cancelled { .. } => Ok(None),
                    Reply::Nobody { .. } => Err(NOBODY_TO_CHOOSE_APP.to_owned()),
                    Reply::Failed { reason, .. } => Err(reason),
                    _ => Err(OUT_OF_TURN.to_owned()),
                });
            }),
        );
    }

    /// Ask the chooser to open `reference` again: the file a window showed, or one inside a
    /// folder the person chose. `write` asks for the file beside a Save, opened for writing.
    pub fn reopen(
        &self,
        reference: &Path,
        write: bool,
        answer: impl FnOnce(Result<(File, PathBuf, String), String>) + Send + 'static,
    ) {
        let asked = reference.to_path_buf();
        self.connection().ask(
            move |id| Request::Reopen {
                id,
                reference: asked,
                write,
            },
            Box::new(move |reply, fds| {
                answer(match reply {
                    Reply::Handed { documents, .. } => {
                        match (documents.into_iter().next(), fds.into_iter().next()) {
                            (Some(document), Some(fd)) => {
                                Ok((File::from(fd), document.reference, document.name))
                            }
                            _ => Err(CHOOSER_OUT_OF_TURN.to_owned()),
                        }
                    }
                    Reply::Cancelled { .. } => Err(CLOSED.to_owned()),
                    Reply::Failed { reason, .. } => Err(reason),
                    _ => Err(CHOOSER_OUT_OF_TURN.to_owned()),
                });
            }),
        );
    }

    /// The bytes of a Save are written: rename the file beside it over it, and
    /// answer whether the file was replaced.
    pub fn saved(
        &self,
        reference: &Path,
        answer: impl FnOnce(Result<(), String>) + Send + 'static,
    ) {
        let asked = reference.to_path_buf();
        self.connection().ask(
            move |id| Request::Saved {
                id,
                reference: asked,
            },
            Box::new(move |reply, _fds| {
                answer(match reply {
                    Reply::Saved { .. } => Ok(()),
                    Reply::Failed { reason, .. } => Err(reason),
                    _ => Err(CHOOSER_OUT_OF_TURN.to_owned()),
                });
            }),
        );
    }

    fn connection(&self) -> &Connection {
        &self.0.connection
    }

    /// The chooser the launcher handed this process, when it handed one. On Windows a process is
    /// handed its connections over its process link instead, and an instance takes its chooser
    /// with them ([`picker`]): no process inherits one.
    #[cfg(windows)]
    pub fn inherited() -> Option<Self> {
        None
    }

    /// The chooser the launcher handed this process (`TILO_SVC_SHELL_FD`), when it handed one.
    #[cfg(unix)]
    pub fn inherited() -> Option<Self> {
        static INHERITED: OnceLock<Option<ShellLink>> = OnceLock::new();
        INHERITED
            .get_or_init(|| {
                let fd = std::env::var(FD_ENV).ok()?.parse::<RawFd>().ok()?;
                let stream = adopt_socket(fd)?;
                Self::connect(stream)
                    .inspect_err(
                        |error| tracing::warn!(%error, "the session's file chooser is unusable"),
                    )
                    .ok()
            })
            .clone()
    }
}

/// The shell's chooser over `stream`, asking `elsewhere` what the session answers `Nobody` for:
/// a session with no portal behind it has nobody to choose a path for an app, and the save
/// directory or the desktop's own chooser still answers a Save.
pub fn picker(
    stream: Stream,
    elsewhere: Arc<dyn FilePicker>,
) -> std::io::Result<Arc<dyn FilePicker>> {
    Ok(ShellLink::connect(stream)?.picker(elsewhere))
}

pub fn inherited_picker(elsewhere: Arc<dyn FilePicker>) -> Option<Arc<dyn FilePicker>> {
    ShellLink::inherited().map(|link| link.picker(elsewhere))
}

pub fn inherited_opener(elsewhere: Arc<dyn crate::UriOpener>) -> Option<Arc<dyn crate::UriOpener>> {
    ShellLink::inherited().map(|link| link.opener(elsewhere))
}

#[cfg(unix)]
fn adopt_socket(fd: RawFd) -> Option<Stream> {
    if fd <= 2 {
        return None;
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: `fstat` only writes the buffer it is given, sized for it.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `fstat` succeeded, so it filled the buffer.
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return None;
    }
    // The launcher hands it over inheritable; the app's own children must
    // not speak for it.
    // SAFETY: setting a descriptor flag on a descriptor we are adopting.
    unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    // SAFETY: the descriptor is an open socket nothing else in this process
    // owns: the launcher placed it for this adoption, which runs once.
    Some(unsafe { Stream::from_raw_fd(fd) })
}

struct ShellChooser {
    link: ShellLink,
    /// What answers a request the session says `Nobody` for: a session with no portal behind it
    /// has nobody to choose a path, and the unattended picker or the desktop's chooser still
    /// answers a Save.
    elsewhere: Arc<dyn FilePicker>,
}

struct ShellOpener {
    link: ShellLink,
    elsewhere: Arc<dyn crate::UriOpener>,
}

impl crate::UriOpener for ShellOpener {
    fn open(&self, uri: &str) -> std::io::Result<()> {
        if let Err(error) = url::Url::parse(uri) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("bad URI: {error}"),
            ));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let asked = uri.to_owned();
        self.link.connection().ask(
            move |id| Request::OpenUri { id, uri: asked },
            Box::new(move |reply, _fds| {
                let _ = tx.send(reply);
            }),
        );
        match rx.recv_timeout(LINK_ANSWER) {
            Ok(Reply::Opened { .. }) => Ok(()),
            Ok(Reply::Unclaimed { .. }) => self.elsewhere.open(uri),
            Ok(Reply::Failed { reason, .. }) if reason == CLOSED || reason == UNREACHABLE => {
                self.elsewhere.open(uri)
            }
            Ok(Reply::Failed { reason, .. }) => Err(std::io::Error::other(reason)),
            Ok(_) => Err(std::io::Error::other(OUT_OF_TURN)),
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the session's shell did not open the link",
            )),
        }
    }
}

fn channel<T: Send + 'static>() -> (impl FnOnce(T) + Send, std::sync::mpsc::Receiver<T>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let wake = tilo_rt::wake::WakeHandle::new();
    let deliver = move |outcome: T| {
        if tx.send(outcome).is_ok() {
            wake.wake();
        }
    };
    (deliver, rx)
}

#[cfg(test)]
mod directory_contract_tests {
    use super::*;

    #[test]
    fn writable_directory_survives_wire_encoding_without_upgrading_read_picks() {
        for (request, expected) in [
            (
                PickRequest::directory("Read folder", "Choose"),
                PickMode::Directory,
            ),
            (
                PickRequest::writable_directory("Import into folder", "Import here"),
                PickMode::WritableDirectory,
            ),
        ] {
            let frame = serde_json::to_vec(&pick_request(&request)(7)).unwrap();
            let Request::Pick {
                mode,
                multiple,
                suggested_name,
                ..
            } = serde_json::from_slice::<Request>(&frame).unwrap()
            else {
                panic!("not a pick")
            };
            assert_eq!(mode, expected);
            assert!(!multiple);
            assert_eq!(suggested_name, None);
        }
    }
}

fn pick_request(request: &PickRequest) -> impl FnOnce(u64) -> Request + use<> {
    let (mode, multiple, suggested_name) = match &request.kind {
        PickKind::Open { multiple } => (PickMode::Open, *multiple, None),
        PickKind::Directory if request.is_writable_directory() => {
            (PickMode::WritableDirectory, false, None)
        }
        PickKind::Directory => (PickMode::Directory, false, None),
        PickKind::Save { suggested_name } => (PickMode::Save, false, Some(suggested_name.clone())),
    };
    let title = request.title.clone();
    let accept_label = request.accept_label.clone();
    let filters = request.filters.iter().map(Filter::from_portal).collect();
    move |id| Request::Pick {
        id,
        mode,
        multiple,
        title,
        accept_label,
        suggested_name,
        filters,
    }
}

impl ShellChooser {
    /// Ask the shell for a pick, and hand the answer to `handed` with the descriptors it came
    /// with; anything the shell does not answer goes to `elsewhere`, as before.
    fn pick_with<T: Send + 'static>(
        &self,
        request: PickRequest,
        deliver: impl FnOnce(T) + Send + 'static,
        handed: impl FnOnce(Vec<HandedDocument>, Vec<Descriptor>) -> T + Send + 'static,
        cancelled: T,
        failed: impl FnOnce(String) -> T + Send + 'static,
        nowhere: impl FnOnce(&dyn FilePicker, PickRequest) -> T + Send + 'static,
    ) {
        let fallback = Arc::clone(&self.elsewhere);
        let build = pick_request(&request);
        self.link.connection().ask(
            build,
            Box::new(move |reply, fds| {
                deliver(match reply {
                    Reply::Handed { documents, .. } if !documents.is_empty() => {
                        if documents.len() == fds.len() {
                            handed(documents, fds)
                        } else {
                            failed(CHOOSER_OUT_OF_TURN.to_owned())
                        }
                    }
                    Reply::Handed { .. } | Reply::Cancelled { .. } => cancelled,
                    Reply::Nobody { .. } => nowhere(&*fallback, request),
                    Reply::Failed { reason, .. } => failed(reason),
                    _ => failed(CHOOSER_OUT_OF_TURN.to_owned()),
                });
            }),
        );
    }
}

/// The file a save writes, and the commit that renames it over the file the person chose.
fn keeping(
    link: &ShellLink,
    document: HandedDocument,
    fd: Descriptor,
) -> Result<OpenedFile, String> {
    let reference = document.reference.clone();
    let link = link.clone();
    let file = handed_file(fd).map_err(|error| error.to_string())?;
    Ok(OpenedFile::keeping(
        file,
        document.name,
        reference.clone(),
        move || {
            commit(&link, &reference);
        },
    ))
}

/// Finish a save: rename the file beside the chosen one over it, and wait briefly for the
/// chooser to answer so the file is there when the write returns.
fn commit(link: &ShellLink, reference: &Path) {
    let (tx, rx) = std::sync::mpsc::channel();
    link.saved(reference, move |outcome| {
        let _ = tx.send(outcome);
    });
    match rx.recv_timeout(SAVE_COMMIT_WAIT) {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => tracing::warn!(%reason, "a save was not committed"),
        Err(_) => tracing::warn!("the chooser did not commit a save in time"),
    }
}

/// Write `bytes` through `file` and commit the save: what a Save's bytes go through, never the
/// file the person sees until they are all there.
fn write_and_commit(
    link: &ShellLink,
    mut file: File,
    reference: &Path,
    bytes: &[u8],
) -> SaveOutcome {
    let written = file
        .set_len(0)
        .and_then(|()| std::io::Write::write_all(&mut file, bytes))
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        return SaveOutcome::Failed(error.to_string());
    }
    let (tx, rx) = std::sync::mpsc::channel();
    link.saved(reference, move |outcome| {
        let _ = tx.send(outcome);
    });
    match rx.recv_timeout(SAVE_COMMIT_WAIT) {
        Ok(Ok(())) => SaveOutcome::Saved,
        Ok(Err(reason)) => SaveOutcome::Failed(reason),
        Err(_) => SaveOutcome::Failed("the chooser did not commit the save in time".to_owned()),
    }
}

fn closed_recv<T>(rx: &std::sync::mpsc::Receiver<T>, closed: T) -> T {
    rx.recv().unwrap_or(closed)
}

fn read_contents(connection: &Connection, grants: Vec<FilePortalGrant>) -> ContentsOutcome {
    let result = (|| {
        if grants.is_empty() || grants.len() > MAX_CONTENT_FILES {
            return Err("invalid selected file grants".to_owned());
        }
        let mut total = 0_u64;
        let mut files = Vec::new();
        for grant in &grants {
            total = total
                .checked_add(grant.size)
                .ok_or("selected files are too large")?;
            if total > MAX_CONTENT_BYTES || grant.token.len() != 36 {
                return Err("selected files are too large or unavailable".to_owned());
            }
            let mut bytes = Vec::new();
            loop {
                let (tx, rx) = std::sync::mpsc::channel();
                let offset = bytes.len() as u64;
                let selected = grant.clone();
                connection.ask(
                    move |id| Request::ReadContents {
                        id,
                        grant: selected,
                        offset,
                        max_bytes: CONTENT_CHUNK_BYTES,
                    },
                    Box::new(move |reply, _fds| {
                        let _ = tx.send(reply);
                    }),
                );
                match rx.recv_timeout(std::time::Duration::from_secs(60)) {
                    Ok(Reply::ContentsChunk {
                        bytes: chunk, eof, ..
                    }) if chunk.len() <= CONTENT_CHUNK_BYTES as usize
                        && offset.saturating_add(chunk.len() as u64) <= grant.size =>
                    {
                        bytes.extend(chunk);
                        if eof {
                            if bytes.len() as u64 != grant.size {
                                return Err("selected file changed".to_owned());
                            }
                            break;
                        }
                        if bytes.len() as u64 == offset {
                            return Err("selected file read made no progress".to_owned());
                        }
                    }
                    Ok(Reply::Failed { reason, .. }) => return Err(reason),
                    _ => return Err("selected file grant is unavailable".to_owned()),
                }
            }
            files.push(PickedContents {
                name: grant.name.clone(),
                media_type: None,
                bytes,
                // What the person chose: a path inside the folder, or the file's name. Never a
                // host path, which the sandbox does not give the app.
                location: Some(std::path::PathBuf::from(&grant.name)),
            });
        }
        Ok(files)
    })();
    for grant in grants {
        connection.ask(
            move |id| Request::ReleaseContents { id, grant },
            Box::new(|_, _fds| {}),
        );
    }
    result.map_or_else(ContentsOutcome::Failed, ContentsOutcome::Picked)
}

fn handed_reference(document: HandedDocument) -> PathBuf {
    document.reference
}

impl FilePicker for ShellChooser {
    fn pick_contents(&self, request: PickRequest) -> ContentsChannel {
        if !request.is_open() && !request.is_directory() {
            return answered(ContentsOutcome::Failed(
                "only an Open or a folder chooser delivers contents".to_owned(),
            ));
        }
        let (deliver, rx) = channel();
        let link = self.link.clone();
        let fallback = Arc::clone(&self.elsewhere);
        let title = request.title.clone();
        let accept_label = request.accept_label.clone();
        let multiple = request.is_multiple();
        let directory = request.is_directory();
        let filters = request.filters.iter().map(Filter::from_portal).collect();
        self.link.connection().ask(
            move |id| Request::PickContents {
                id,
                multiple,
                directory,
                title,
                accept_label,
                filters,
            },
            Box::new(move |reply, _fds| {
                let spawned = tilo_rt::background::Background::current().spawn(
                    "tilo-portal-contents",
                    move || {
                        let outcome = match reply {
                            Reply::ContentsGranted { grants, .. } => {
                                read_contents(link.connection(), grants)
                            }
                            Reply::Cancelled { .. } => ContentsOutcome::Cancelled,
                            Reply::Nobody { .. } => closed_recv(
                                &fallback.pick_contents(request),
                                ContentsOutcome::Failed(CLOSED.to_owned()),
                            ),
                            Reply::Failed { reason, .. } => ContentsOutcome::Failed(reason),
                            _ => ContentsOutcome::Failed(CHOOSER_OUT_OF_TURN.to_owned()),
                        };
                        deliver(outcome);
                    },
                );
                if let Err(error) = spawned {
                    tracing::warn!(%error, "cannot read selected file grant");
                }
            }),
        );
        rx
    }

    fn pick(&self, request: PickRequest) -> PortalChannel {
        let (deliver, rx) = channel();
        self.pick_with(
            request,
            deliver,
            |documents, _fds| {
                documents
                    .into_iter()
                    .next()
                    .map_or(PortalOutcome::Cancelled, |document| {
                        PortalOutcome::Opened(handed_reference(document))
                    })
            },
            PortalOutcome::Cancelled,
            PortalOutcome::Failed,
            |picker, request| {
                closed_recv(&picker.pick(request), PortalOutcome::Failed(CLOSED.into()))
            },
        );
        rx
    }

    fn pick_multiple(&self, request: PickRequest) -> MultiChannel {
        let (deliver, rx) = channel();
        self.pick_with(
            request,
            deliver,
            |documents, _fds| {
                MultiOutcome::Selected(documents.into_iter().map(handed_reference).collect())
            },
            MultiOutcome::Cancelled,
            MultiOutcome::Failed,
            |picker, request| {
                closed_recv(
                    &picker.pick_multiple(request),
                    MultiOutcome::Failed(CLOSED.into()),
                )
            },
        );
        rx
    }

    fn open_file(&self, request: PickRequest) -> OpenFileChannel {
        if !matches!(request.kind, PickKind::Open { multiple: false }) {
            return answered(OpenFileOutcome::Failed(
                "an already-open file request must be a single-file Open chooser".to_string(),
            ));
        }
        self.open_handle(request)
    }

    /// The chosen file or folder, opened by the descriptor the chooser handed over, so the app
    /// reads it in place on a host where its path names nothing.
    fn open_handle(&self, request: PickRequest) -> OpenFileChannel {
        if !matches!(
            request.kind,
            PickKind::Open { multiple: false } | PickKind::Directory
        ) {
            return answered(OpenFileOutcome::Failed(
                "a handle request must be a single-file Open or a folder chooser".to_string(),
            ));
        }
        let (deliver, rx) = channel();
        self.pick_with(
            request,
            deliver,
            |documents, fds| match (documents.into_iter().next(), fds.into_iter().next()) {
                (Some(document), Some(fd)) => {
                    OpenFileOutcome::Opened(OpenedFile::at(File::from(fd), &document.reference))
                }
                _ => OpenFileOutcome::Failed(CHOOSER_OUT_OF_TURN.to_owned()),
            },
            OpenFileOutcome::Cancelled,
            OpenFileOutcome::Failed,
            |picker, request| {
                closed_recv(
                    &picker.open_handle(request),
                    OpenFileOutcome::Failed(CLOSED.into()),
                )
            },
        );
        rx
    }

    /// A Save handed as a write descriptor: the app's writes land in the file beside the chosen
    /// one, and dropping the file renames it over the file the person chose.
    fn save_file(&self, request: PickRequest) -> OpenFileChannel {
        if request.suggested_name().is_none() {
            return answered(OpenFileOutcome::Failed(
                "a save request names a file".to_string(),
            ));
        }
        let (deliver, rx) = channel();
        let link = self.link.clone();
        self.pick_with(
            request,
            deliver,
            move |documents, fds| match (documents.into_iter().next(), fds.into_iter().next()) {
                (Some(document), Some(fd)) => match keeping(&link, document, fd) {
                    Ok(opened) => OpenFileOutcome::Opened(opened),
                    Err(reason) => OpenFileOutcome::Failed(reason),
                },
                _ => OpenFileOutcome::Failed(CHOOSER_OUT_OF_TURN.to_owned()),
            },
            OpenFileOutcome::Cancelled,
            OpenFileOutcome::Failed,
            |picker, request| {
                closed_recv(
                    &picker.save_file(request),
                    OpenFileOutcome::Failed(CLOSED.into()),
                )
            },
        );
        rx
    }

    /// A Save whose bytes are written through the descriptor, then committed by the chooser.
    fn save_contents(&self, request: PickRequest, bytes: Vec<u8>) -> SaveChannel {
        if request.suggested_name().is_none() {
            return answered(SaveOutcome::Failed(
                "only a Save chooser saves contents".to_owned(),
            ));
        }
        let (deliver, rx) = channel();
        let link = self.link.clone();
        let fallback = Arc::clone(&self.elsewhere);
        let build = pick_request(&request);
        self.link.connection().ask(
            build,
            Box::new(move |reply, fds| {
                let outcome = match reply {
                    Reply::Handed { documents, .. } => {
                        match (documents.into_iter().next(), fds.into_iter().next()) {
                            (Some(document), Some(fd)) => {
                                write_and_commit(&link, File::from(fd), &document.reference, &bytes)
                            }
                            _ => SaveOutcome::Failed(CHOOSER_OUT_OF_TURN.to_owned()),
                        }
                    }
                    Reply::Cancelled { .. } => SaveOutcome::Cancelled,
                    Reply::Nobody { .. } => closed_recv(
                        &fallback.save_contents(request, bytes),
                        SaveOutcome::Failed(CLOSED.to_owned()),
                    ),
                    Reply::Failed { reason, .. } => SaveOutcome::Failed(reason),
                    _ => SaveOutcome::Failed(CHOOSER_OUT_OF_TURN.to_owned()),
                };
                deliver(outcome);
            }),
        );
        rx
    }

    /// A destination the app writes to again on every later Save, named by its reference: each
    /// write reopens the file beside the chosen one and commits it over the file.
    fn save_destination(&self, request: PickRequest) -> crate::portal::DestinationChannel {
        if request.suggested_name().is_none() {
            return crate::portal::DestinationChannel::answered(
                crate::portal::DestinationOutcome::Failed(
                    "only a Save chooser chooses where to save".to_owned(),
                ),
            );
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let link = self.link.clone();
        let fallback = Arc::clone(&self.elsewhere);
        let build = pick_request(&request);
        self.link.connection().ask(
            build,
            Box::new(move |reply, fds| {
                let outcome = match reply {
                    Reply::Handed { documents, .. } => {
                        match (documents.into_iter().next(), fds.into_iter().next()) {
                            (Some(document), Some(fd)) => {
                                let first = Arc::new(Mutex::new(Some(File::from(fd))));
                                let reference = document.reference.clone();
                                let name = document.name;
                                let writer = {
                                    let link = link.clone();
                                    let reference = reference.clone();
                                    move |bytes: Vec<u8>| {
                                        let link = link.clone();
                                        let reference = reference.clone();
                                        let taken =
                                            first.lock().ok().and_then(|mut first| first.take());
                                        let (send, receive) = std::sync::mpsc::channel();
                                        let spawned = tilo_rt::background::Background::current()
                                            .spawn("tilo-shell-save", move || {
                                                let outcome = match taken {
                                                    Some(file) => write_and_commit(
                                                        &link, file, &reference, &bytes,
                                                    ),
                                                    None => {
                                                        reopen_and_write(&link, &reference, &bytes)
                                                    }
                                                };
                                                let _ = send.send(outcome);
                                            });
                                        if spawned.is_err() {
                                            let (send, receive) = std::sync::mpsc::channel();
                                            let _ = send.send(SaveOutcome::Failed(
                                                "this host runs no background work".to_owned(),
                                            ));
                                            return receive;
                                        }
                                        receive
                                    }
                                };
                                crate::portal::DestinationOutcome::Chosen(
                                    crate::portal::SaveDestination::kept_at(
                                        name, reference, writer,
                                    ),
                                )
                            }
                            _ => crate::portal::DestinationOutcome::Failed(
                                CHOOSER_OUT_OF_TURN.to_owned(),
                            ),
                        }
                    }
                    Reply::Cancelled { .. } => crate::portal::DestinationOutcome::Cancelled,
                    Reply::Nobody { .. } => {
                        let outcome =
                            fallback
                                .save_destination(request)
                                .recv()
                                .unwrap_or_else(|_| {
                                    crate::portal::DestinationOutcome::Failed(CLOSED.to_owned())
                                });
                        let _ = tx.send(outcome);
                        return;
                    }
                    Reply::Failed { reason, .. } => {
                        crate::portal::DestinationOutcome::Failed(reason)
                    }
                    _ => crate::portal::DestinationOutcome::Failed(CHOOSER_OUT_OF_TURN.to_owned()),
                };
                let _ = tx.send(outcome);
            }),
        );
        crate::portal::DestinationChannel::from_outcomes(rx)
    }

    fn launch(&self, request: LaunchRequest) -> LaunchChannel {
        if !request.path.is_absolute() {
            return answered(LaunchOutcome::Failed(
                "desktop-open path must be absolute".into(),
            ));
        }
        if request.is_reveal() {
            return answered(LaunchOutcome::Failed(
                "this session cannot show a file's folder".into(),
            ));
        }
        let (deliver, rx) = channel();
        let path = request.path;
        self.link.connection().ask(
            move |id| Request::Open { id, path },
            Box::new(move |reply, _fds| {
                deliver(match reply {
                    Reply::Opened { .. } => LaunchOutcome::Opened,
                    Reply::Failed { reason, .. } => LaunchOutcome::Failed(reason),
                    _ => LaunchOutcome::Failed(CHOOSER_OUT_OF_TURN.into()),
                });
            }),
        );
        rx
    }
}

/// Reopen the file a Save writes and commit `bytes`: the place a destination writes to again.
fn reopen_and_write(link: &ShellLink, reference: &Path, bytes: &[u8]) -> SaveOutcome {
    let (tx, rx) = std::sync::mpsc::channel();
    link.reopen(reference, true, move |opened| {
        let _ = tx.send(opened);
    });
    match rx.recv_timeout(LINK_ANSWER) {
        Ok(Ok((file, held, _name))) => write_and_commit(link, file, &held, bytes),
        Ok(Err(reason)) => SaveOutcome::Failed(reason),
        Err(_) => SaveOutcome::Failed("the chooser did not answer in time".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, BufReader, Read as _};
    use std::path::Path;
    use std::time::Duration;

    fn answer() -> Duration {
        tilo_test_env::hang_bound()
    }

    /// A reply the shell writes, with the files it hands over.
    struct Answer(Reply, Vec<std::fs::File>);

    impl Answer {
        fn reply(reply: Reply) -> Self {
            Self(reply, Vec::new())
        }

        /// A choice of `documents`, each opened in `file`.
        fn handed(id: u64, documents: &[(&str, std::fs::File)]) -> Self {
            Self(
                Reply::Handed {
                    id,
                    documents: documents
                        .iter()
                        .map(|(reference, _)| HandedDocument {
                            reference: PathBuf::from(reference),
                            name: Path::new(reference)
                                .file_name()
                                .map_or_else(String::new, |name| {
                                    name.to_string_lossy().into_owned()
                                }),
                        })
                        .collect(),
                },
                documents
                    .iter()
                    .map(|(_, file)| file.try_clone().unwrap())
                    .collect(),
            )
        }
    }

    fn shell(respond: impl Fn(Request) -> Answer + Send + 'static) -> Stream {
        let (app, shell) = tilo_socket::pair().unwrap();
        std::thread::spawn(move || {
            let writer = shell.try_clone().unwrap();
            for line in BufReader::new(shell).lines() {
                let Ok(line) = line else { break };
                let request: Request = serde_json::from_str(&line).unwrap();
                let Answer(reply, files) = respond(request);
                let mut frame = serde_json::to_vec(&reply).unwrap();
                frame.push(b'\n');
                #[cfg(unix)]
                let fds: Vec<Descriptor> = files.into_iter().map(OwnedFd::from).collect();
                #[cfg(not(unix))]
                let fds: Vec<Descriptor> = {
                    drop(files);
                    Vec::new()
                };
                if send_frame(&writer, &frame, &fds).is_err() {
                    break;
                }
            }
        });
        app
    }

    fn id_of(request: &Request) -> u64 {
        match request {
            Request::Pick { id, .. }
            | Request::Open { id, .. }
            | Request::OpenUri { id, .. }
            | Request::PickContents { id, .. }
            | Request::ReadContents { id, .. }
            | Request::ReleaseContents { id, .. }
            | Request::Reopen { id, .. }
            | Request::Saved { id, .. }
            | Request::ChooseApp { id, .. }
            | Request::AwaitIdentity { id }
            | Request::ChooseIdentity { id } => *id,
        }
    }

    #[cfg(unix)]
    #[test]
    fn only_an_inherited_socket_is_adopted() {
        use std::os::fd::IntoRawFd as _;

        let file = tempfile::tempfile().unwrap().into_raw_fd();
        assert!(adopt_socket(file).is_none(), "a file is not the chooser");
        // SAFETY: the test owns `file` and nothing adopted it.
        drop(unsafe { std::fs::File::from_raw_fd(file) });
        assert!(adopt_socket(2).is_none());

        let (app, mut shell) = tilo_socket::pair().unwrap();
        let adopted = adopt_socket(app.into_raw_fd()).expect("a socket is adopted");
        // SAFETY: reading the flags of a descriptor the test owns.
        let flags =
            unsafe { libc::fcntl(std::os::fd::AsRawFd::as_raw_fd(&adopted), libc::F_GETFD) };
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "the app's children don't inherit it"
        );
        drop(adopted);
        let mut rest = Vec::new();
        shell.read_to_end(&mut rest).unwrap();
        assert!(rest.is_empty(), "dropping the adopted socket closes it");
    }

    tilo_test_env::scenario! {
        fn an_app_asks_its_shell_and_reports_the_shells_answer;

        phase the_shell_chooser_keeps_the_picker_contract;
        {
            let chooser = picker(
                shell(|request| {
                    Answer::reply(Reply::Failed {
                        id: id_of(&request),
                        reason: "no".into(),
                    })
                }),
                crate::unavailable_picker(),
            )
            .unwrap();
            crate::conformance::check_picker_contract(&*chooser);
        }

        phase a_pick_carries_the_request_and_delivers_what_the_shell_chose;
        {
            let (seen_tx, seen) = std::sync::mpsc::channel();
            let chooser = picker(
                shell(move |request| {
                    let id = id_of(&request);
                    let _ = seen_tx.send(request);
                    let with = |name: &str| {
                        let file = tempfile::tempfile().unwrap();
                        (name.to_owned(), file)
                    };
                    let documents = [with("/data/in/x.xlsx"), with("/data/in/y.xlsx")];
                    Answer::handed(
                        id,
                        &documents
                            .iter()
                            .map(|(name, file)| (name.as_str(), file.try_clone().unwrap()))
                            .collect::<Vec<_>>(),
                    )
                }),
                crate::unavailable_picker(),
            )
            .unwrap();
            let request = PickRequest::open("Import spreadsheet", "Import")
                .filter(FileFilter::new("Excel").glob("*.xlsx"));
            assert_eq!(
                chooser.pick(request).recv_timeout(answer()).unwrap(),
                PortalOutcome::Opened("/data/in/x.xlsx".into())
            );
            let Request::Pick {
                mode,
                multiple,
                title,
                accept_label,
                suggested_name,
                filters,
                ..
            } = seen.recv_timeout(answer()).unwrap()
            else {
                panic!("a chooser request is a pick");
            };
            assert_eq!(mode, PickMode::Open);
            assert!(!multiple);
            assert_eq!(title, "Import spreadsheet");
            assert_eq!(accept_label, "Import");
            assert_eq!(suggested_name, None);
            assert_eq!(
                filters.first().map(|f| f.globs.clone()),
                Some(vec!["*.xlsx".to_owned()])
            );

            assert_eq!(
                chooser
                    .pick_multiple(PickRequest::open_multiple("Open", "Open"))
                    .recv_timeout(answer())
                    .unwrap(),
                MultiOutcome::Selected(vec!["/data/in/x.xlsx".into(), "/data/in/y.xlsx".into()])
            );
            let Request::Pick { multiple, .. } = seen.recv_timeout(answer()).unwrap() else {
                panic!("a chooser request is a pick");
            };
            assert!(multiple);
        }

        phase nobody_at_the_computer_falls_back_to_the_unattended_picker;
        {
            let directory = tempfile::tempdir().unwrap();
            let unattended =
                crate::unattended_picker(directory.path().to_path_buf(), crate::unavailable_picker());
            let nobody = |request: Request| {
                Answer::reply(Reply::Nobody {
                    id: id_of(&request),
                })
            };
            let chooser = picker(shell(nobody), unattended.expect("an unattended picker")).unwrap();
            assert_eq!(
                chooser
                    .pick(PickRequest::save("Save", "Save", "report.xlsx"))
                    .recv_timeout(answer())
                    .unwrap(),
                PortalOutcome::Opened(directory.path().join("report.xlsx"))
            );
            assert!(matches!(
                chooser.pick(PickRequest::open("Open", "Open")).recv_timeout(answer()),
                Ok(PortalOutcome::Failed(reason)) if reason.contains("nobody")
            ));

            // An app that brought no chooser of its own says plainly there is none.
            let alone = picker(shell(nobody), crate::unavailable_picker()).unwrap();
            assert!(matches!(
                alone
                    .pick(PickRequest::save("Save", "Save", "report.xlsx"))
                    .recv_timeout(answer()),
                Ok(PortalOutcome::Failed(_))
            ));
        }

        phase opening_a_file_asks_the_shell_and_reports_its_answer;
        {
            let chooser = picker(
                shell(|request| {
                    Answer::reply(match request {
                        Request::Open { id, path } if path == Path::new("/data/in/a.docx") => {
                            Reply::Opened { id }
                        }
                        other => Reply::Failed {
                            id: id_of(&other),
                            reason: "not granted".into(),
                        },
                    })
                }),
                crate::unavailable_picker(),
            )
            .unwrap();
            assert_eq!(
                chooser
                    .launch(LaunchRequest::open("/data/in/a.docx"))
                    .recv_timeout(answer())
                    .unwrap(),
                LaunchOutcome::Opened
            );
            assert_eq!(
                chooser
                    .launch(LaunchRequest::open("/etc/shadow"))
                    .recv_timeout(answer())
                    .unwrap(),
                LaunchOutcome::Failed("not granted".into())
            );
        }

        phase the_app_chooser_answers_the_app_chosen_or_none;
        {
            let terminal = ChosenApp {
                id: "org.example.Terminal".into(),
                name: "Terminal".into(),
                window_ids: vec!["org.example.Terminal".into(), "terminal".into()],
            };
            let answer_with = terminal.clone();
            let link = ShellLink::connect(shell(move |request| {
                Answer::reply(match request {
                    Request::ChooseApp { id, purpose } if purpose == "Paste in apps" => {
                        Reply::AppChosen {
                            id,
                            app: answer_with.clone(),
                        }
                    }
                    other => Reply::Cancelled { id: id_of(&other) },
                })
            }))
            .unwrap();
            let (chosen_tx, chosen) = std::sync::mpsc::channel();
            let tx = chosen_tx.clone();
            link.choose_app("Paste in apps", move |outcome| tx.send(outcome).unwrap());
            assert_eq!(chosen.recv_timeout(answer()).unwrap(), Ok(Some(terminal)));
            link.choose_app("Something else", move |outcome| {
                chosen_tx.send(outcome).unwrap();
            });
            assert_eq!(chosen.recv_timeout(answer()).unwrap(), Ok(None));
        }

        phase a_link_opens_through_the_shell_and_only_what_it_leaves_goes_elsewhere;
        {
            let elsewhere = Arc::new(Elsewhere::default());
            let opener = ShellLink::connect(shell(|request| {
                Answer::reply(match request {
                    Request::OpenUri { id, uri } if uri.starts_with("settings:") => {
                        Reply::Opened { id }
                    }
                    Request::OpenUri { id, uri } if uri.starts_with("mailto:") => {
                        Reply::Unclaimed { id }
                    }
                    other => Reply::Failed {
                        id: id_of(&other),
                        reason: "the app could not start".into(),
                    },
                })
            }))
            .unwrap()
            .opener(Arc::clone(&elsewhere) as Arc<dyn crate::UriOpener>);
            crate::conformance::check_opener_contract(&*opener);

            opener.open("settings://updates").unwrap();
            assert!(elsewhere.0.lock().unwrap().is_empty());
            opener.open("mailto:ada@example.test").unwrap();
            assert_eq!(*elsewhere.0.lock().unwrap(), ["mailto:ada@example.test"]);
            let refused = opener.open("web+notes:today").unwrap_err();
            assert_eq!(refused.to_string(), "the app could not start");
            assert_eq!(
                elsewhere.0.lock().unwrap().len(),
                1,
                "a refusal is reported, not rerouted"
            );

            let (app, gone) = tilo_socket::pair().unwrap();
            drop(gone);
            let alone = ShellLink::connect(app)
                .unwrap()
                .opener(Arc::clone(&elsewhere) as Arc<dyn crate::UriOpener>);
            alone.open("https://example.test/").unwrap();
            assert_eq!(
                elsewhere.0.lock().unwrap().last().map(String::as_str),
                Some("https://example.test/"),
                "without its shell an app's links still open"
            );
        }

        phase a_cancelled_pick_and_a_gone_shell_answer_instead_of_hanging;
        {
            let chooser = picker(
                shell(|request| {
                    Answer::reply(Reply::Cancelled {
                        id: id_of(&request),
                    })
                }),
                crate::unavailable_picker(),
            )
            .unwrap();
            assert_eq!(
                chooser
                    .pick(PickRequest::open("Open", "Open"))
                    .recv_timeout(answer())
                    .unwrap(),
                PortalOutcome::Cancelled
            );

            let (app, shell) = tilo_socket::pair().unwrap();
            let chooser = picker(app, crate::unavailable_picker()).unwrap();
            let waiting = chooser.pick(PickRequest::open("Open", "Open"));
            drop(shell);
            assert!(matches!(
                waiting.recv_timeout(answer()),
                Ok(PortalOutcome::Failed(_))
            ));
            assert!(matches!(
                chooser
                    .launch(LaunchRequest::open("/data/in/a.docx"))
                    .recv_timeout(answer()),
                Ok(LaunchOutcome::Failed(_))
            ));
        }
    }

    #[derive(Default)]
    struct Elsewhere(Mutex<Vec<String>>);

    impl crate::UriOpener for Elsewhere {
        fn open(&self, uri: &str) -> std::io::Result<()> {
            self.0.lock().unwrap().push(uri.to_owned());
            Ok(())
        }
    }
}
