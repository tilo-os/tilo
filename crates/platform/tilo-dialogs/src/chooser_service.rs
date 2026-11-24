//! The server side of the shell's chooser protocol ([`crate::shell`]).
//!
//! One server answers every app's connection the same way, wherever it runs: a desktop session
//! serves it on the launcher's socket, and a device daemon serves it on a socket pair it makes
//! for the tilolang host it starts. What differs is only what the host fills in
//! ([`Serving`]): where the person is asked, and how a link opens in an app.
//!
//! The person's choice is the grant. The file or folder is added to the documents store under
//! the app's key, and the answer carries a descriptor opened with the person's rights, so an
//! app in a sandbox reads and writes exactly what was chosen without a host path. The reference
//! the answer also carries names the same document again later ([`crate::shell::Request::Reopen`]);
//! a save writes its bytes into a file beside the chosen one, which the chooser renames over the
//! file only when the app says they are all there ([`crate::shell::Request::Saved`]).

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::hash::{BuildHasher as _, RandomState};
use std::io::Write as _;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tilo_documents::{AppKey, Document, Documents, Kind};

use crate::portal::FileFilter;
use crate::shell::{
    CONTENT_CHUNK_BYTES, FilePortalGrant, Filter, FrameReader, HandedDocument, Handshake,
    MAX_CONTENT_BYTES, MAX_CONTENT_FILES, MAX_FRAME_BYTES, PROTOCOL, PickMode, Reply, Request,
    send_frame,
};

/// How many selected files' contents one connection holds open at once.
const MAX_HELD: usize = 4 * MAX_CONTENT_FILES;
/// Names an app or a workspace may have on the handshake.
const MAX_NAME: usize = 255;

// What an app shows the person when the chooser could not do what it asked: what happened, and
// what to do.
const CANNOT_HAND: &str = "Files can't be handed to this app on this computer.";
const DOES_NOT_FIT: &str =
    "The file chooser's answer doesn't fit what the app asked for. Try again.";
const TOO_LARGE: &str =
    "Those files are too large for the app to read. Choose fewer or smaller files.";
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
    /// The files people choose for apps.
    pub documents: Documents,
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
    pub multiple: bool,
    pub accept_label: Option<String>,
    pub current_name: Option<String>,
    pub filters: Vec<FileFilter>,
}

impl Question {
    /// What a choice answering this question gives the app.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        if self.save {
            Kind::Save
        } else if self.directory {
            Kind::Folder {
                write: self.writable_directory,
            }
        } else {
            Kind::Read
        }
    }
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
        held: Mutex::new(HashMap::new()),
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
            Request::ReadContents {
                id,
                grant,
                offset,
                max_bytes,
            } => connection.say(&read_contents(&connection, id, &grant, offset, max_bytes)),
            Request::ReleaseContents { id, grant } => {
                connection
                    .held
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&grant.token);
                connection.say(&Reply::ContentsReleased { id });
            }
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
            request @ (Request::Pick { .. } | Request::PickContents { .. }) => {
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
    // The app is gone: what it held is let go.
    connection
        .held
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
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
static TOKENS: AtomicU64 = AtomicU64::new(0);

/// One app's connection: who it is, as its launcher named it, and what it holds.
struct Connection {
    serving: Arc<Serving>,
    /// Which connection this is, for its questions' handles.
    number: u64,
    app: String,
    workspace: String,
    writer: Mutex<UnixStream>,
    /// The selected files whose contents the app may still read, by grant token.
    held: Mutex<HashMap<String, Held>>,
}

/// A selected file the app may read the bytes of. The path stays in this host: the app holds only
/// the grant's token, and a folder of ten thousand files leaves no handle open between reads.
struct Held {
    request: u64,
    name: String,
    size: u64,
    path: PathBuf,
}

/// What a pick hands the app once the person chose.
#[derive(Clone, Copy)]
enum Handing {
    /// The chosen items themselves, each with a descriptor and a reference.
    Documents,
    /// Only the chosen files' bytes, on the connection.
    Contents,
}

/// The question a pick asks, and what it hands over; `None` for a request that is no pick.
fn pick_question(request: Request) -> Option<(u64, Question, Handing)> {
    match request {
        Request::Pick {
            id,
            mode,
            multiple,
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
                multiple,
                accept_label: Some(accept_label).filter(|label| !label.is_empty()),
                current_name: suggested_name.filter(|name| !name.is_empty()),
                filters: filters.iter().map(filter_of).collect(),
            },
            Handing::Documents,
        )),
        Request::PickContents {
            id,
            multiple,
            directory,
            title,
            accept_label,
            filters,
        } => Some((
            id,
            Question {
                title,
                save: false,
                directory,
                writable_directory: false,
                multiple: multiple && !directory,
                accept_label: Some(accept_label).filter(|label| !label.is_empty()),
                current_name: None,
                filters: filters.iter().map(filter_of).collect(),
            },
            Handing::Contents,
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

    fn say_with(&self, reply: &Reply, fds: &[OwnedFd]) {
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

    /// The app's documents key, as the launcher computed it when it bound the app's view.
    fn key(&self) -> Option<AppKey> {
        AppKey::new(&self.workspace, &self.app)
    }

    /// Ask the person what `request` asks for, and answer the app once they chose. Their answer
    /// can take as long as they like, so the question runs on a thread of its own and nothing on
    /// this connection waits for it: the app's other requests are answered meanwhile.
    fn ask_person(connection: &Arc<Self>, request: Request) {
        let Some((id, question, handing)) = pick_question(request) else {
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
            Ok(paths) => match handing {
                Handing::Documents => {
                    let (reply, fds) = asking.hand(id, &paths, question.kind());
                    asking.say_with(&reply, &fds);
                    return;
                }
                Handing::Contents => asking.contents(id, &paths, question.directory),
            },
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
            | Request::PickContents { id, .. }
            | Request::Reopen { id, .. }
            | Request::Saved { id, .. }
            | Request::ReadContents { id, .. }
            | Request::ReleaseContents { id, .. }
            | Request::ChooseApp { id, .. }
            | Request::AwaitIdentity { id }
            | Request::ChooseIdentity { id } => failed(id, DOES_NOT_FIT),
        }
    }

    /// The chosen items themselves: added to the app's documents store, and handed with a
    /// descriptor opened with the person's rights. Never a host path.
    fn hand(&self, id: u64, paths: &[PathBuf], kind: Kind) -> (Reply, Vec<OwnedFd>) {
        let Some(key) = self.key() else {
            return (failed(id, CANNOT_HAND), Vec::new());
        };
        let mut documents = Vec::with_capacity(paths.len());
        let mut fds = Vec::with_capacity(paths.len());
        for path in paths {
            let reference = match self.serving.documents.grant(&key, path, kind) {
                Ok(reference) => reference,
                Err(error) => {
                    tracing::debug!(%error, path = %path.display(), "a chosen item is not kept for the app");
                    return (failed(id, DOES_NOT_FIT), Vec::new());
                }
            };
            let Some((token, _)) = tilo_documents::split_sandbox_path(&reference) else {
                return (failed(id, DOES_NOT_FIT), Vec::new());
            };
            let Some(document) = self.serving.documents.document(&key, &token) else {
                return (failed(id, DOES_NOT_FIT), Vec::new());
            };
            match open_document(&document, kind) {
                Ok(fd) => {
                    documents.push(HandedDocument {
                        reference,
                        name: document.name().to_string_lossy().into_owned(),
                    });
                    fds.push(fd);
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

    /// The contents of what the person chose, `paths`: opened here, and only their bytes handed
    /// on, on this connection.
    fn contents(&self, id: u64, paths: &[PathBuf], directory: bool) -> Reply {
        if directory {
            let Some(root) = paths.first() else {
                return failed(id, TOO_LARGE);
            };
            return self.folder(id, root);
        }
        if paths.len() > MAX_CONTENT_FILES {
            return failed(id, TOO_LARGE);
        }
        let mut opened = Vec::with_capacity(paths.len());
        let mut total = 0_u64;
        for path in paths {
            let name = path.file_name().map_or_else(
                || "file".to_owned(),
                |name| name.to_string_lossy().into_owned(),
            );
            let Ok(file) = crate::portal::open_regular_file(path) else {
                return failed(id, &format!("“{name}” can't be read. Choose another file."));
            };
            let Ok(size) = file.metadata().map(|metadata| metadata.len()) else {
                return failed(id, &format!("“{name}” can't be read. Choose another file."));
            };
            total = total.saturating_add(size);
            if total > MAX_CONTENT_BYTES {
                return failed(id, TOO_LARGE);
            }
            opened.push((name, size, path.clone(), file));
        }
        self.granted(id, opened)
    }

    /// The files of the folder `root`, each under its path inside it: what a folder choice hands
    /// the app, so it reads a library as it would have chosen the files one by one. Symlinks are
    /// skipped, the walk is bounded, and a folder that cannot be read refuses the whole choice
    /// rather than handing over a part of it.
    fn folder(&self, id: u64, root: &Path) -> Reply {
        let mut opened = Vec::new();
        let mut total = 0_u64;
        let mut stack = vec![(root.to_path_buf(), PathBuf::new())];
        while let Some((folder, relative)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&folder) else {
                let name = folder.to_string_lossy();
                return failed(
                    id,
                    &format!("“{name}” can't be read. Choose another folder."),
                );
            };
            let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                if opened.len() >= MAX_CONTENT_FILES {
                    return failed(id, TOO_LARGE);
                }
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let inside = relative.join(entry.file_name());
                if kind.is_dir() {
                    stack.push((entry.path(), inside));
                    continue;
                }
                // Only a regular file is a file the person chose; a link or a pipe is not.
                if !kind.is_file() {
                    continue;
                }
                let Ok(file) = crate::portal::open_regular_file(&entry.path()) else {
                    continue;
                };
                let Ok(size) = file.metadata().map(|metadata| metadata.len()) else {
                    continue;
                };
                total = total.saturating_add(size);
                if total > MAX_CONTENT_BYTES {
                    return failed(id, TOO_LARGE);
                }
                let path = inside.to_string_lossy().replace('\\', "/");
                opened.push((path, size, entry.path(), file));
            }
        }
        if opened.is_empty() {
            return failed(id, "That folder holds no files to read.");
        }
        self.granted(id, opened)
    }

    /// The grants for the files a choice hands over, named by where they stand in what the
    /// person chose.
    fn granted(&self, id: u64, opened: Vec<(String, u64, PathBuf, File)>) -> Reply {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if held.len().saturating_add(opened.len()) > MAX_HELD {
            return failed(id, TOO_LARGE);
        }
        let grants = opened
            .into_iter()
            .map(|(name, size, path, _file)| {
                let token = token();
                held.insert(
                    token.clone(),
                    Held {
                        request: id,
                        name: name.clone(),
                        size,
                        path,
                    },
                );
                FilePortalGrant {
                    request: id,
                    token,
                    name,
                    size,
                }
            })
            .collect();
        drop(held);
        Reply::ContentsGranted { id, grants }
    }

    /// Turn a reference back into an open descriptor: the file a window showed, or a file inside
    /// a folder the person chose. Only a document of this app is reached.
    fn reopen(&self, id: u64, reference: &Path, write: bool) -> (Reply, Vec<OwnedFd>) {
        let opened = (|| {
            let key = self.key().ok_or(NOT_CHOSEN)?;
            let (token, inner) = tilo_documents::split_sandbox_path(reference).ok_or(NOT_CHOSEN)?;
            let document = self
                .serving
                .documents
                .document(&key, &token)
                .ok_or(NOT_CHOSEN)?;
            let within = inner
                .strip_prefix(document.name())
                .map_err(|_| NOT_CHOSEN)?;
            let relative = plain_relative(within).ok_or(NOT_CHOSEN)?;
            let file = if relative.as_os_str().is_empty() {
                File::from(
                    open_document(&document, if write { document.kind } else { Kind::Read })
                        .map_err(|_| NOT_CHOSEN)?,
                )
            } else {
                if !matches!(document.kind, Kind::Folder { .. }) {
                    return Err(NOT_CHOSEN);
                }
                if write && !document.kind.writes() {
                    return Err(NOT_CHOSEN);
                }
                open_beneath(&document.host, &relative, write)?
            };
            let name = relative.file_name().map_or_else(
                || document.name().to_string_lossy().into_owned(),
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
                vec![fd],
            ),
            Err(reason) => (failed(id, reason), Vec::new()),
        }
    }

    /// A save's bytes are in the file beside it: rename that over the file the person chose.
    fn saved(&self, id: u64, reference: &Path) -> Reply {
        let renamed = (|| {
            let key = self.key().ok_or(NOT_CHOSEN)?;
            let (token, inner) = tilo_documents::split_sandbox_path(reference).ok_or(NOT_CHOSEN)?;
            if !inner.as_os_str().is_empty() {
                return Err(NOT_CHOSEN);
            }
            let document = self
                .serving
                .documents
                .document(&key, &token)
                .ok_or(NOT_CHOSEN)?;
            if document.kind != Kind::Save {
                return Err(NOT_CHOSEN);
            }
            let beside = document.save_sibling().ok_or(NOT_CHOSEN)?;
            std::fs::rename(document.host.with_file_name(beside), &document.host).map_err(|_| GONE)
        })();
        match renamed {
            Ok(()) => Reply::Saved { id },
            Err(reason) => failed(id, reason),
        }
    }

    /// The host file a reference names, for opening it in an app: one the person chose for it.
    fn opened_path(&self, path: &Path) -> Result<PathBuf, &'static str> {
        let key = self.key().ok_or(NOT_CHOSEN)?;
        let (token, inner) = tilo_documents::split_sandbox_path(path).ok_or(NOT_CHOSEN)?;
        let document = self
            .serving
            .documents
            .document(&key, &token)
            .ok_or(NOT_CHOSEN)?;
        let within = inner
            .strip_prefix(document.name())
            .map_err(|_| NOT_CHOSEN)?;
        if within.as_os_str().is_empty() {
            return Ok(document.host);
        }
        plain_relative(within).ok_or(NOT_CHOSEN)?;
        // Inside a chosen folder, and only a folder.
        if matches!(document.kind, Kind::Folder { .. }) {
            Ok(document.host.join(within))
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

/// The next bytes of a selected file the app holds a grant to.
fn read_contents(
    connection: &Connection,
    id: u64,
    grant: &FilePortalGrant,
    offset: u64,
    max_bytes: u32,
) -> Reply {
    let held = {
        let held = connection
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        held.get(&grant.token)
            .filter(|held| {
                held.request == grant.request && held.size == grant.size && held.name == grant.name
            })
            .map(|held| (held.path.clone(), held.size))
    };
    let Some((path, size)) = held else {
        return failed(id, GONE);
    };
    if offset > size {
        return failed(id, GONE);
    }
    let Ok(file) = crate::portal::open_regular_file(&path) else {
        return failed(id, GONE);
    };
    // A file that changed since it was chosen is no longer the one the person chose.
    if !file.metadata().is_ok_and(|metadata| metadata.len() == size) {
        return failed(id, GONE);
    }
    let room = size.saturating_sub(offset);
    let want = u64::from(max_bytes.min(CONTENT_CHUNK_BYTES)).min(room);
    let mut bytes = vec![0; usize::try_from(want).unwrap_or(0)];
    if file.read_exact_at(&mut bytes, offset).is_err() {
        return failed(id, GONE);
    }
    let eof = offset.saturating_add(want) >= size;
    Reply::ContentsChunk { id, bytes, eof }
}

/// The open descriptor for a chosen document, as `kind` gives it: a file to read or write, or a
/// folder.
fn open_document(document: &Document, kind: Kind) -> Result<OwnedFd, std::io::Error> {
    let file = match kind {
        Kind::Read => crate::portal::open_regular_file(&document.host)?,
        Kind::Folder { .. } => crate::portal::open_directory_handle(&document.host)?,
        Kind::Save => open_for_save(document)?,
    };
    Ok(OwnedFd::from(file))
}

fn open_for_save(document: &Document) -> std::io::Result<File> {
    let beside = document.save_sibling().ok_or_else(|| {
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
        .open(document.host.with_file_name(beside))
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

fn token() -> String {
    format!(
        "{:x}-{:x}",
        RandomState::new().hash_one(TOKENS.fetch_add(1, Ordering::Relaxed)),
        std::process::id()
    )
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
