//! The server side of the shell's chooser protocol ([`crate::shell`]).
//!
//! One server answers every app's connection the same way, wherever it runs: a desktop session
//! serves it on the launcher's socket, and a device daemon serves it on a socket pair it makes
//! for the tilolang host it starts. What differs is only what the host fills in
//! ([`Serving`]): where the person is asked, and how a link opens in an app.
//!
//! The person's choice is the grant: the host keeps the chosen file or folder for the app
//! ([`Grants`]), and the answer carries a descriptor opened with the person's rights, so an app
//! in a sandbox reads and writes exactly what was chosen without a host path. The reference the
//! answer also carries names the same document again later ([`crate::shell::Request::Reopen`]);
//! a save writes its bytes into a file beside the chosen one, which the chooser renames over the
//! file only when the app says they are all there ([`crate::shell::Request::Saved`]).

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::portal::FileFilter;
use crate::shell::{
    Filter, FrameReader, HandedDocument, Handshake, MAX_FRAME_BYTES, PROTOCOL, PickMode, Reply,
    Request, send_frame,
};
/// Names an app or a workspace may have on the handshake.
const MAX_NAME: usize = 255;

// What an app shows the person when the chooser could not do what it asked: what happened, and
// what to do.
pub const CANNOT_HAND: &str = "Files can't be handed to this app on this computer.";
pub const DOES_NOT_FIT: &str =
    "The file chooser's answer doesn't fit what the app asked for. Try again.";
const GONE: &str = "That file is no longer available to the app. Choose it again.";
pub const NOT_CHOSEN: &str = "This app can only open files you chose for it.";
pub const NOTHING_OPENS: &str = "Nothing on this computer opens this file.";
const BAD_LINK: &str = "That link isn't a web address or anything else that can be opened.";
const DID_NOT_OPEN: &str = "It didn't open. Try again.";
pub const NO_APP_CHOOSER: &str = "This computer can't ask you to choose an app yet.";
const NO_IDENTITY_CHOOSER: &str = "This computer can't ask you to choose an identity yet.";
const NOT_ASKED: &str = "The file chooser couldn't be shown. Try again.";

/// What a chooser server is served with.
#[derive(Clone)]
pub struct Serving {
    /// Where the person is asked; `None` when nobody is here to answer one.
    pub person: Option<Arc<dyn Person>>,
    /// What a choice gives the app and what it reopens: the host's document records.
    pub grants: Arc<dyn Grants>,
    /// What opens a link or a chosen file in an app.
    pub opens: Arc<dyn Opens>,
}

/// Who asks, as the chooser knows an app: the launcher's name for it, and its workspace.
#[derive(Debug, Clone)]
pub struct Caller {
    pub app: String,
    pub workspace: String,
}

/// The question a pick asks the person, in the chooser's own words: the same fields on every
/// host, whatever draws the sheet.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is one of the chooser's independent options"
)]
pub struct Question {
    pub title: String,
    pub save: bool,
    pub directory: bool,
    pub writable_directory: bool,
    /// The chosen file is opened for writing as well as reading: what a document app's Open
    /// asks, so the document saves back where it was opened.
    pub edit: bool,
    pub multiple: bool,
    pub accept_label: Option<String>,
    pub current_name: Option<String>,
    pub filters: Vec<FileFilter>,
}

impl Question {
    /// What a choice answering this question gives the app.
    #[must_use]
    pub const fn choice(&self) -> Choice {
        if self.save {
            Choice::Save
        } else if self.directory {
            Choice::Folder {
                write: self.writable_directory,
            }
        } else if self.edit {
            Choice::Edit
        } else {
            Choice::Read
        }
    }
}

/// What a choice allows the app: one file to read, one file to read and replace when a save
/// ends, or a folder and everything under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Read,
    /// A file with a document's words: read as it stands, and replaced by a save.
    Edit,
    Folder {
        write: bool,
    },
    Save,
}

impl Choice {
    /// Whether the choice lets the app change what it names.
    #[must_use]
    pub const fn writes(self) -> bool {
        matches!(self, Self::Save | Self::Edit | Self::Folder { write: true })
    }
}

/// One document the person gave the app, as the chooser opens it.
#[derive(Debug, Clone)]
pub struct Granted {
    /// The path the app reaches it by inside its sandbox.
    pub reference: PathBuf,
    /// Its name there.
    pub name: String,
    /// The host file or folder behind the reference.
    pub host: PathBuf,
    /// What the choice allows.
    pub choice: Choice,
    /// Where a save writes before its bytes replace the file.
    pub save_sibling: Option<std::ffi::OsString>,
}

/// Where the chooser's documents and their grants live. The host answers it, so the chooser
/// reaches a person's choices through the host's records instead of a store of its own.
pub trait Grants: Send + Sync {
    /// Keep what the person chose for `caller` as `choice`, answering the document to hand over.
    ///
    /// # Errors
    /// What the app shows the person when the choice cannot be kept.
    fn grant(&self, caller: &Caller, chosen: &Path, choice: Choice) -> Result<Granted, String>;

    /// The document `reference` names for `caller`, if the person gave that app this file.
    fn document(&self, caller: &Caller, reference: &Path) -> Option<Granted>;
}

/// Why the person gave no files: they dismissed the sheet, or it could not be shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    Cancelled,
    Failed,
}

/// Where a host asks the person.
pub trait Person: Send + Sync {
    /// Ask the person `question` for `caller` under `handle`, and answer the host paths they
    /// chose. Blocks for as long as the person takes.
    ///
    /// # Errors
    /// [`Asked::Cancelled`] when they chose nothing, [`Asked::Failed`] when the sheet could not
    /// be shown.
    fn choose(
        &self,
        caller: &Caller,
        handle: String,
        question: &Question,
    ) -> Result<Vec<PathBuf>, Asked>;
}

/// Where a link or a chosen file opens in an app.
pub trait Opens: Send + Sync {
    /// Open `uri` in `workspace` in the app that takes it; `None` when nothing does.
    ///
    /// # Errors
    /// The app that takes it did not start.
    fn open(&self, workspace: &str, uri: &str) -> Result<Option<()>, String>;
}

/// Open `stream` as one app's chooser connection: the launcher's handshake first, then what the
/// app asks for. The connection is served on the calling thread, so callers run it on one of
/// their own.
pub fn serve_connection(serving: Arc<Serving>, stream: UnixStream) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let mut reader = FrameReader::new(stream);
    let Some(handshake) = read_handshake(&mut reader) else {
        tracing::warn!("a chooser connection named no app: it is closed");
        return;
    };
    let connection = Arc::new(Connection {
        serving,
        number: CONNECTIONS.fetch_add(1, Ordering::Relaxed),
        app: handshake.app_id,
        workspace: handshake.workspace_id,
        writer: Mutex::new(writer),
    });
    loop {
        let frame = match reader.line() {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(app = %connection.app, %error, "a chooser request is unreadable");
                break;
            }
        };
        let request = match serde_json::from_slice::<Request>(frame.trim_ascii_end()) {
            Ok(request) => request,
            Err(error) => {
                tracing::warn!(app = %connection.app, %error, "a chooser request does not decode");
                break;
            }
        };
        match request {
            Request::Reopen {
                id,
                reference,
                write,
            } => {
                let (reply, fds) = connection.reopen(id, &reference, write);
                connection.say_with(&reply, &fds);
            }
            Request::Saved { id, reference } => connection.say(&connection.saved(id, &reference)),
            Request::ChooseApp { id, .. } => connection.say(&Reply::Failed {
                id,
                reason: NO_APP_CHOOSER.to_owned(),
            }),
            Request::AwaitIdentity { id } | Request::ChooseIdentity { id } => {
                connection.say(&Reply::Failed {
                    id,
                    reason: NO_IDENTITY_CHOOSER.to_owned(),
                });
            }
            request @ Request::Pick { .. } => {
                Connection::ask_person(&connection, request);
            }
            waits => {
                let asking = Arc::clone(&connection);
                if let Err(error) = std::thread::Builder::new()
                    .name("tilo-chooser-ask".into())
                    .spawn(move || {
                        let reply = asking.answer(waits);
                        asking.say(&reply);
                    })
                {
                    tracing::warn!(%error, "a chooser request could not be answered");
                }
            }
        }
    }
}

/// Accept connections until the listener closes.
pub fn serve_listener(listener: &UnixListener, serving: &Arc<Serving>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let serving = Arc::clone(serving);
                if let Err(error) = std::thread::Builder::new()
                    .name("tilo-chooser-app".into())
                    .spawn(move || serve_connection(serving, stream))
                {
                    tracing::warn!(%error, "a chooser connection could not be served");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                tracing::warn!(%error, "the chooser service's accept failed");
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
}

/// A socket pair for one app: the caller's end, already named by the handshake, and the server
/// serving the other end on a thread of its own. What a device daemon hands a tilolang host it
/// starts, in place of a session's socket.
///
/// # Errors
/// The pair or the serving thread cannot be made.
pub fn pair(serving: Serving, app: &str, workspace: &str) -> std::io::Result<UnixStream> {
    let (app_end, runtime_end) = UnixStream::pair()?;
    let handshake = Handshake {
        protocol: PROTOCOL,
        app_id: app.to_owned(),
        workspace_id: workspace.to_owned(),
    };
    let mut frame = serde_json::to_vec(&handshake).map_err(std::io::Error::other)?;
    frame.push(b'\n');
    let mut writer = app_end.try_clone()?;
    writer.write_all(&frame)?;
    writer.flush()?;
    let serving = Arc::new(serving);
    std::thread::Builder::new()
        .name("tilo-chooser".into())
        .spawn(move || serve_connection(serving, runtime_end))?;
    Ok(app_end)
}

static CONNECTIONS: AtomicU64 = AtomicU64::new(0);

/// One app's connection: who it is, as its launcher named it, and what it holds.
struct Connection {
    serving: Arc<Serving>,
    /// Which connection this is, for its questions' handles.
    number: u64,
    app: String,
    workspace: String,
    writer: Mutex<UnixStream>,
}

/// The question a pick asks; `None` for a request that is no pick.
fn pick_question(request: Request) -> Option<(u64, Question)> {
    match request {
        Request::Pick {
            id,
            mode,
            multiple,
            edit,
            title,
            accept_label,
            suggested_name,
            filters,
        } => Some((
            id,
            Question {
                title,
                save: mode == PickMode::Save,
                directory: matches!(mode, PickMode::Directory | PickMode::WritableDirectory),
                writable_directory: mode == PickMode::WritableDirectory,
                edit,
                multiple,
                accept_label: Some(accept_label).filter(|label| !label.is_empty()),
                current_name: suggested_name.filter(|name| !name.is_empty()),
                filters: filters.iter().map(filter_of).collect(),
            },
        )),
        _ => None,
    }
}

fn filter_of(filter: &Filter) -> FileFilter {
    let mut made = FileFilter::new(&filter.name);
    for glob in &filter.globs {
        made = made.glob(glob);
    }
    for mimetype in &filter.mimetypes {
        made = made.mimetype(mimetype);
    }
    made
}

/// The launcher's handshake, the connection's first line.
fn read_handshake(reader: &mut FrameReader) -> Option<Handshake> {
    let frame = reader.line().ok()??;
    let handshake: Handshake = serde_json::from_slice(frame.trim_ascii_end()).ok()?;
    let named = |name: &str| !name.is_empty() && name.len() <= MAX_NAME;
    (handshake.protocol == PROTOCOL && named(&handshake.app_id) && named(&handshake.workspace_id))
        .then_some(handshake)
}

impl Connection {
    /// Write `reply` as its line; a reply the wire cannot carry is logged, and an app that stopped
    /// reading has gone.
    fn say(&self, reply: &Reply) {
        self.say_with(reply, &[]);
    }

    fn say_with(&self, reply: &Reply, fds: &[crate::shell::Descriptor]) {
        let Ok(mut frame) = serde_json::to_vec(reply) else {
            return;
        };
        if frame.len() >= MAX_FRAME_BYTES {
            tracing::warn!(app = %self.app, "a chooser reply is larger than the wire allows");
            return;
        }
        frame.push(b'\n');
        let writer = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(error) = send_frame(&writer, &frame, fds) {
            tracing::debug!(%error, app = %self.app, "an app closed its end of the chooser");
        }
    }

    /// Who asks, as the launcher named it on the handshake.
    fn caller(&self) -> Caller {
        Caller {
            app: self.app.clone(),
            workspace: self.workspace.clone(),
        }
    }

    /// Ask the person what `request` asks for, and answer the app once they chose. Their answer
    /// can take as long as they like, so the question runs on a thread of its own and nothing on
    /// this connection waits for it: the app's other requests are answered meanwhile.
    fn ask_person(connection: &Arc<Self>, request: Request) {
        let Some((id, question)) = pick_question(request) else {
            return;
        };
        let Some(person) = connection.serving.person.clone() else {
            return connection.say(&Reply::Nobody { id });
        };
        let caller = Caller {
            app: connection.app.clone(),
            workspace: connection.workspace.clone(),
        };
        let handle = format!("chooser:{}:{id}", connection.number);
        let asking = Arc::clone(connection);
        let chosen = person.choose(&caller, handle, &question);
        let reply = match chosen {
            Ok(paths) => {
                let (reply, fds) = asking.hand(id, &paths, question.choice());
                asking.say_with(&reply, &fds);
                return;
            }
            Err(Asked::Cancelled) => Reply::Cancelled { id },
            Err(Asked::Failed) => failed(id, NOT_ASKED),
        };
        asking.say(&reply);
    }

    /// Answer a request that waits for another app: opening a link or a file.
    fn answer(&self, request: Request) -> Reply {
        match request {
            Request::Open { id, path } => match self.opened_path(&path) {
                Ok(host) => match file_uri(&host) {
                    Some(uri) => match self.open(&uri) {
                        Ok(true) => Reply::Opened { id },
                        Ok(false) => failed(id, NOTHING_OPENS),
                        Err(reason) => failed(id, reason),
                    },
                    None => failed(id, NOT_CHOSEN),
                },
                Err(reason) => failed(id, reason),
            },
            Request::OpenUri { id, uri } => self.open_uri(id, &uri),
            Request::Pick { id, .. }
            | Request::Reopen { id, .. }
            | Request::Saved { id, .. }
            | Request::ChooseApp { id, .. }
            | Request::AwaitIdentity { id }
            | Request::ChooseIdentity { id }
            | Request::PickContents { id, .. }
            | Request::ReadContents { id, .. }
            | Request::ReleaseContents { id, .. } => failed(id, DOES_NOT_FIT),
        }
    }

    /// The chosen items themselves: kept for the app by its host, and handed with a descriptor
    /// opened with the person's rights. Never a host path.
    fn hand(
        &self,
        id: u64,
        paths: &[PathBuf],
        choice: Choice,
    ) -> (Reply, Vec<crate::shell::Descriptor>) {
        let caller = self.caller();
        let mut documents = Vec::with_capacity(paths.len());
        let mut fds = Vec::with_capacity(paths.len());
        for path in paths {
            let granted = match self.serving.grants.grant(&caller, path, choice) {
                Ok(granted) => granted,
                Err(reason) => {
                    tracing::debug!(%reason, path = %path.display(), "a chosen item is not kept for the app");
                    return (failed(id, &reason), Vec::new());
                }
            };
            match open_document(&granted, choice) {
                Ok(fd) => {
                    documents.push(HandedDocument {
                        reference: granted.reference,
                        name: granted.name,
                    });
                    fds.push(crate::shell::Descriptor::from(fd));
                }
                Err(error) => {
                    tracing::debug!(%error, "a chosen item cannot be opened for the app");
                    return (failed(id, DOES_NOT_FIT), Vec::new());
                }
            }
        }
        if documents.is_empty() {
            return (failed(id, DOES_NOT_FIT), Vec::new());
        }
        (Reply::Handed { id, documents }, fds)
    }

    /// Turn a reference back into an open descriptor: the file a window showed, or a file inside
    /// a folder the person chose. Only a document of this app is reached.
    fn reopen(
        &self,
        id: u64,
        reference: &Path,
        write: bool,
    ) -> (Reply, Vec<crate::shell::Descriptor>) {
        let opened = (|| {
            let caller = self.caller();
            let granted = self
                .serving
                .grants
                .document(&caller, reference)
                .ok_or(NOT_CHOSEN)?;
            let within = reference
                .strip_prefix(&granted.reference)
                .map_err(|_| NOT_CHOSEN)?;
            let relative = plain_relative(within).ok_or(NOT_CHOSEN)?;
            let file = if relative.as_os_str().is_empty() {
                // A folder opens as a folder, to read what it holds; a file to read unless the
                // app asked to write what its choice lets it.
                let choice = match granted.choice {
                    Choice::Folder { write: may } => Choice::Folder {
                        write: write && may,
                    },
                    // A document given to be edited is written back beside itself and renamed
                    // over it, as a save writes: read when it is opened again to read.
                    Choice::Edit | Choice::Save if write => Choice::Save,
                    Choice::Edit | Choice::Read | Choice::Save if !write => Choice::Read,
                    // A file given to read is never opened for writing.
                    _ => return Err(NOT_CHOSEN),
                };
                File::from(open_document(&granted, choice).map_err(|_| NOT_CHOSEN)?)
            } else {
                if !matches!(granted.choice, Choice::Folder { .. }) {
                    return Err(NOT_CHOSEN);
                }
                if write && !granted.choice.writes() {
                    return Err(NOT_CHOSEN);
                }
                open_beneath(&granted.host, &relative, write)?
            };
            let name = relative.file_name().map_or_else(
                || granted.name.clone(),
                |name| name.to_string_lossy().into_owned(),
            );
            Ok((
                OwnedFd::from(file),
                HandedDocument {
                    reference: reference.to_path_buf(),
                    name,
                },
            ))
        })();
        match opened {
            Ok((fd, document)) => (
                Reply::Handed {
                    id,
                    documents: vec![document],
                },
                vec![crate::shell::Descriptor::from(fd)],
            ),
            Err(reason) => (failed(id, reason), Vec::new()),
        }
    }

    /// A save's bytes are in the file beside it: rename that over the file the person chose,
    /// answering the place it now is.
    fn saved(&self, id: u64, reference: &Path) -> Reply {
        let renamed = (|| {
            let caller = self.caller();
            let granted = self
                .serving
                .grants
                .document(&caller, reference)
                .ok_or(NOT_CHOSEN)?;
            if !matches!(granted.choice, Choice::Save | Choice::Edit)
                || granted.reference != reference
            {
                return Err(NOT_CHOSEN);
            }
            let beside = granted.save_sibling.ok_or(NOT_CHOSEN)?;
            std::fs::rename(granted.host.with_file_name(beside), &granted.host)
                .map_err(|_| GONE)?;
            Ok(granted.host.display().to_string())
        })();
        match renamed {
            Ok(to) => Reply::Saved { id, to },
            Err(reason) => failed(id, reason),
        }
    }

    /// The host file a reference names, for opening it in an app: one the person chose for it.
    fn opened_path(&self, path: &Path) -> Result<PathBuf, &'static str> {
        let granted = self
            .serving
            .grants
            .document(&self.caller(), path)
            .ok_or(NOT_CHOSEN)?;
        let within = path
            .strip_prefix(&granted.reference)
            .map_err(|_| NOT_CHOSEN)?;
        if within.as_os_str().is_empty() {
            return Ok(granted.host);
        }
        plain_relative(within).ok_or(NOT_CHOSEN)?;
        // Inside a chosen folder, and only a folder.
        if matches!(granted.choice, Choice::Folder { .. }) {
            Ok(granted.host.join(within))
        } else {
            Err(NOT_CHOSEN)
        }
    }

    /// Open `uri` the way a link the app asks to open goes: a file only as [`Self::opened_path`]
    /// allows, anything else in the app that claims its scheme.
    fn open_uri(&self, id: u64, uri: &str) -> Reply {
        let Ok(parsed) = url::Url::parse(uri) else {
            return failed(id, BAD_LINK);
        };
        if parsed.scheme() == "file" {
            let Ok(path) = parsed.to_file_path() else {
                return failed(id, BAD_LINK);
            };
            let host = match self.opened_path(&path) {
                Ok(host) => host,
                Err(reason) => return failed(id, reason),
            };
            let Some(uri) = file_uri(&host) else {
                return failed(id, NOT_CHOSEN);
            };
            return match self.open(&uri) {
                Ok(true) => Reply::Opened { id },
                Ok(false) => failed(id, NOTHING_OPENS),
                Err(reason) => failed(id, reason),
            };
        }
        match self.open(uri) {
            Ok(true) => Reply::Opened { id },
            Ok(false) => Reply::Unclaimed { id },
            Err(reason) => failed(id, reason),
        }
    }

    /// Ask the host to open `uri` in the app's workspace: whether an app took it.
    fn open(&self, uri: &str) -> Result<bool, &'static str> {
        match self.serving.opens.open(&self.workspace, uri) {
            Ok(opened) => Ok(opened.is_some()),
            Err(error) => {
                tracing::warn!(app = %self.app, %error, "what an app asked to open did not open");
                Err(DID_NOT_OPEN)
            }
        }
    }
}

/// The open descriptor for a chosen document, as `choice` gives it: a file to read or write, or a
/// folder.
fn open_document(granted: &Granted, choice: Choice) -> Result<OwnedFd, std::io::Error> {
    let file = match choice {
        Choice::Read | Choice::Edit => crate::portal::open_regular_file(&granted.host)?,
        Choice::Folder { .. } => crate::portal::open_directory_handle(&granted.host)?,
        Choice::Save => open_for_save(granted)?,
    };
    Ok(OwnedFd::from(file))
}

fn open_for_save(granted: &Granted) -> std::io::Result<File> {
    let beside = granted.save_sibling.clone().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "a save writes beside a file",
        )
    })?;
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(granted.host.with_file_name(beside))
}

/// `relative` with every component a plain name: what opens beneath a chosen folder, never a
/// name that climbs out of it.
fn plain_relative(relative: &Path) -> Option<PathBuf> {
    let mut plain = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(name) => plain.push(name),
            _ => return None,
        }
    }
    Some(plain)
}

/// Open a file inside a chosen folder, walking it a component at a time with `openat`, never
/// following a link: the person chose the folder and what is under it, not where a link points.
fn open_beneath(root: &Path, relative: &Path, write: bool) -> Result<File, &'static str> {
    let directory = crate::portal::open_directory_handle(root).map_err(|_| NOT_CHOSEN)?;
    let mut current = OwnedFd::from(directory);
    let parts: Vec<&std::ffi::OsStr> = relative.iter().collect();
    let count = parts.len();
    for (at, part) in parts.into_iter().enumerate() {
        let last = at.saturating_add(1) == count;
        let flags = if last && write {
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else if last {
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };
        let name = CString::new(part.as_bytes()).map_err(|_| NOT_CHOSEN)?;
        // SAFETY: `openat` reads the descriptor and the name, both valid for the call.
        let fd = unsafe { libc::openat(current.as_raw_fd(), name.as_ptr(), flags, 0o644) };
        if fd < 0 {
            return Err(NOT_CHOSEN);
        }
        // SAFETY: `openat` just returned this descriptor, which nothing else owns.
        current = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    let file = File::from(current);
    if write && !file.metadata().map_err(|_| NOT_CHOSEN)?.is_file() {
        return Err(NOT_CHOSEN);
    }
    Ok(file)
}

/// `path` as an RFC 8089 file URI.
fn file_uri(path: &Path) -> Option<String> {
    url::Url::from_file_path(path).ok().map(String::from)
}

fn failed(id: u64, reason: &str) -> Reply {
    Reply::Failed {
        id,
        reason: reason.to_owned(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::collections::HashMap;
    use std::io::Read as _;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use super::*;
    use crate::portal::{OpenFileOutcome, PickRequest, SaveOutcome};

    /// A person who chooses what the test put in front of them.
    struct Chosen(PathBuf);

    impl Person for Chosen {
        fn choose(
            &self,
            _caller: &Caller,
            _handle: String,
            _question: &Question,
        ) -> Result<Vec<PathBuf>, Asked> {
            Ok(vec![self.0.clone()])
        }
    }

    /// A host keeping what was chosen: a reference names it again, and nothing else does.
    #[derive(Default)]
    struct Kept(Mutex<HashMap<PathBuf, Granted>>);

    impl Grants for Kept {
        fn grant(
            &self,
            _caller: &Caller,
            chosen: &Path,
            choice: Choice,
        ) -> Result<Granted, String> {
            let name = chosen
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .ok_or_else(|| "the choice names no file".to_owned())?;
            let reference = Path::new("/run/tilo/chosen/records")
                .join("test")
                .join(&name);
            let save_sibling = matches!(choice, Choice::Save | Choice::Edit).then(|| {
                let mut sibling = std::ffi::OsString::from(".");
                sibling.push(&name);
                sibling.push(".tilo-save");
                sibling
            });
            let granted = Granted {
                reference: reference.clone(),
                name,
                host: chosen.to_path_buf(),
                choice,
                save_sibling,
            };
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(reference, granted.clone());
            Ok(granted)
        }

        fn document(&self, _caller: &Caller, reference: &Path) -> Option<Granted> {
            let kept = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            kept.iter()
                .find(|(given, _)| reference.starts_with(given))
                .map(|(_, granted)| granted.clone())
        }
    }

    struct NoOpen;

    impl Opens for NoOpen {
        fn open(&self, _workspace: &str, _uri: &str) -> Result<Option<()>, String> {
            Ok(None)
        }
    }

    #[test]
    fn a_choice_hands_its_descriptor_and_a_reference_it_alone_reopens() {
        let directory = tempfile::tempdir().unwrap();
        let chosen = directory.path().join("plan.txt");
        std::fs::write(&chosen, "Spring planting").unwrap();
        let serving = Serving {
            person: Some(Arc::new(Chosen(chosen))),
            grants: Arc::new(Kept::default()),
            opens: Arc::new(NoOpen),
        };
        let stream = pair(
            serving,
            "so.tilo.notes",
            "5ba25f73-87c5-447f-a5a6-880dc4249a81",
        )
        .unwrap();
        let picker = crate::shell::picker(stream, crate::unavailable_picker()).unwrap();

        // The chosen file comes as a descriptor opened with the person's rights, and a
        // reference that names it again.
        let opened = picker
            .open_handle(PickRequest::open("Import a plan", "Import"))
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
        let OpenFileOutcome::Opened(opened) = opened else {
            panic!("the chosen file was not handed over");
        };
        assert_eq!(opened.display_name(), "plan.txt");
        let reference = opened
            .reference()
            .expect("a reference names it")
            .to_path_buf();
        let mut file = opened.into_file();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "Spring planting");

        // The app reopens what it was given …
        let reopened = picker
            .reopen(&reference, false)
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
        let OpenFileOutcome::Opened(reopened) = reopened else {
            panic!("a reference the app was given reopens");
        };
        let mut file = reopened.into_file();
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "Spring planting");

        // … and a reference, or a host path, it was never given opens nothing.
        for never in [
            PathBuf::from("/etc/passwd"),
            directory.path().join("never.txt"),
        ] {
            let refused = picker
                .reopen(&never, false)
                .recv_timeout(tilo_test_env::hang_bound())
                .unwrap();
            assert!(
                matches!(refused, OpenFileOutcome::Failed(_)),
                "{} must not be reachable through the chooser",
                never.display()
            );
        }
    }

    #[test]
    fn a_save_writes_beside_the_file_and_commits_over_it() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("report.txt");
        let serving = Serving {
            person: Some(Arc::new(Chosen(destination.clone()))),
            grants: Arc::new(Kept::default()),
            opens: Arc::new(NoOpen),
        };
        let stream = pair(
            serving,
            "so.tilo.notes",
            "5ba25f73-87c5-447f-a5a6-880dc4249a81",
        )
        .unwrap();
        let picker = crate::shell::picker(stream, crate::unavailable_picker()).unwrap();

        let saving = picker.save_contents(
            PickRequest::save("Save the report", "Save", "report.txt"),
            b"the report".to_vec(),
        );
        let outcome = saving.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert_eq!(
            outcome,
            SaveOutcome::Saved {
                to: destination.display().to_string()
            },
            "the chooser commits the save"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"the report");

        let mut left: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["report.txt"],
            "the bytes are written beside the file and the file is the only thing left"
        );
    }

    #[test]
    fn a_file_opened_for_editing_is_written_back_over_itself() {
        let directory = tempfile::tempdir().unwrap();
        let document = directory.path().join("Letter.docx");
        std::fs::write(&document, "first draft").unwrap();
        let serving = Serving {
            person: Some(Arc::new(Chosen(document.clone()))),
            grants: Arc::new(Kept::default()),
            opens: Arc::new(NoOpen),
        };
        let stream = pair(
            serving,
            "so.tilo.word-processor",
            "5ba25f73-87c5-447f-a5a6-880dc4249a81",
        )
        .unwrap();
        let picker = crate::shell::picker(stream, crate::unavailable_picker()).unwrap();

        // A document app's Open asks for the file opened for writing too.
        let opened = picker
            .open_handle(PickRequest::open("Open a document", "Open").edit(true))
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
        let OpenFileOutcome::Opened(opened) = opened else {
            panic!("the chosen document was not handed over");
        };
        let reference = opened
            .reference()
            .expect("a reference names it")
            .to_path_buf();
        drop(opened.into_file());

        // Saving writes the document back over the file the person chose, whole, and leaves
        // nothing beside it.
        let reopened = picker
            .reopen(&reference, true)
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
        let OpenFileOutcome::Opened(opened) = reopened else {
            panic!("a document opened for editing reopens for writing");
        };
        let mut held = opened.into_file();
        held.write_whole(b"second draft")
            .expect("the document is written back");
        drop(held);
        picker
            .commit_saved(&reference)
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap()
            .expect("the save is committed");
        assert_eq!(std::fs::read(&document).unwrap(), b"second draft");
        let mut left: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        left.sort();
        assert_eq!(left, ["Letter.docx"], "the save leaves nothing beside it");

        // A file the person chose to read alone is not written back over.
        let read = picker
            .open_handle(PickRequest::open("Read a document", "Open"))
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
        let OpenFileOutcome::Opened(read) = read else {
            panic!("the chosen document was not handed over");
        };
        let read_reference = read
            .reference()
            .expect("a reference names it")
            .to_path_buf();
        drop(read.into_file());
        let refused = picker
            .reopen(&read_reference, true)
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
        assert!(
            matches!(refused, OpenFileOutcome::Failed(_)),
            "a file given to read is not written over"
        );
    }
}
