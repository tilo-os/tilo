//! The shell's chooser service (`tilo_dialogs::shell`): one system service every app reaches for
//! the files it asks the person for and the links and files it opens.
//!
//! The launcher connects an app to this socket before its sandbox starts and writes the first
//! line, a [`Handshake`] naming the app and its workspace; nothing the app writes after can change
//! who it is. A file the app asks for is chosen in the session's own file chooser, the same
//! question the desktop portal asks ([`tilo_portal::file_chooser::choose`]), and handed to the app
//! the same way: a path in its view of the documents store, or, for contents, only the bytes. A
//! link or a file it opens goes to the app that takes it here, the way the shell's own search
//! opens one ([`crate::apps::Apps::open_claimed`]), and a link nothing takes is answered
//! [`Reply::Unclaimed`], so the app hands it to its desktop.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, ErrorKind, Write as _};
use std::os::unix::fs::FileExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::ApiError;
use tilo_dialogs::shell::{
    CONTENT_CHUNK_BYTES, FilePortalGrant, Filter, Handshake, MAX_CONTENT_BYTES, MAX_CONTENT_FILES,
    MAX_FRAME_BYTES, PROTOCOL, PickMode, Reply, Request,
};
use tilo_lock::MutexRecover as _;
use tilo_portal::PortalCaller;
use tilo_portal::file_chooser::{FileChooserQuestion, PickerFilter};
use tilo_portal::reply::Ended;
use tilo_service::{Questions, Wake};

use crate::Launches;

/// How long an open waits for the session's loop, which alone starts apps. The app waits a little
/// longer for the answer than this.
const OPEN_WAIT: Duration = Duration::from_secs(20);
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
const NOT_CHOSEN: &str = "This app can only open files you chose for it.";
const NOTHING_OPENS: &str = "Nothing on this computer opens this file.";
const BAD_LINK: &str = "That link isn't a web address or anything else that can be opened.";
const DID_NOT_OPEN: &str = "It didn't open. Try again.";
const NOT_IN_TIME: &str = "It didn't open in time. Try again.";
const NO_APP_CHOOSER: &str = "This computer can't ask you to choose an app yet.";
const NO_IDENTITY_CHOOSER: &str = "This computer can't ask you to choose an identity yet.";
const NOT_ASKED: &str = "The file chooser couldn't be shown. Try again.";

/// What the chooser service is served with.
pub(crate) struct Serving {
    /// The questions the session's sheets show; `None` when nobody is here to answer one.
    pub questions: Option<Questions>,
    /// The files people choose for apps, while the session serves them to its sandboxes.
    pub documents: Option<tilo_documents::Documents>,
    /// The apps this session started: which of them run with the documents store bound in.
    pub launches: Arc<Launches>,
    /// What wakes the session's loop when an open waits for it.
    pub wake: Wake,
}

/// An open the service leaves for the session's loop: only a pass of the loop decides which app
/// takes a link and starts it.
pub(crate) struct Open {
    workspace: String,
    uri: String,
    reply: SyncSender<Result<Option<()>, ApiError>>,
}

/// What opens a link or a file for the loop: the session's apps.
pub(crate) trait Opens {
    /// Open `uri` in `workspace` in the app that takes it; `None` when nothing does.
    ///
    /// # Errors
    /// The app that takes it did not start.
    fn open_claimed(&self, workspace: &str, uri: &str) -> Result<Option<()>, ApiError>;
}

impl Opens for crate::apps::Apps {
    fn open_claimed(&self, workspace: &str, uri: &str) -> Result<Option<()>, ApiError> {
        Self::open_claimed(self, workspace, uri)
    }
}

/// What the session's loop and the connections share.
struct Shared {
    serving: Serving,
    opens: Mutex<Vec<Open>>,
    connections: AtomicU64,
}

/// The chooser service, served until the session ends.
pub(crate) struct Served {
    shared: Arc<Shared>,
}

impl Served {
    /// Open what the apps asked to open since the last pass, on the loop.
    pub(crate) fn follow(&self, opens: &dyn Opens) {
        let asked = std::mem::take(&mut *self.shared.opens.lock_recover());
        for open in asked {
            let _ = open
                .reply
                .send(opens.open_claimed(&open.workspace, &open.uri));
        }
    }
}

/// Bind the chooser socket under the launcher's services root and serve it.
///
/// # Errors
/// The services root or the socket cannot be made, or the acceptor cannot start.
pub(crate) fn serve(serving: Serving) -> Result<Served, String> {
    launcher::services::ensure_services_root()
        .map_err(|error| format!("create the services directory: {error}"))?;
    serve_at(
        &launcher::services::socket_for(launcher::services::CHOOSER_SERVICE),
        serving,
    )
}

#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the chooser service accepts and serves on its own threads"
)]
fn serve_at(path: &Path, serving: Serving) -> Result<Served, String> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|error| format!("remove the stale chooser socket: {error}"))?;
    }
    let listener = tilo_files::native::socket::bind(path)
        .map_err(|error| format!("bind the chooser socket {}: {error}", path.display()))?;
    let shared = Arc::new(Shared {
        serving,
        opens: Mutex::new(Vec::new()),
        connections: AtomicU64::new(0),
    });
    let accepting = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("tilo-chooser".into())
        .spawn(move || accept(&listener, &accepting))
        .map_err(|error| format!("start the chooser service: {error}"))?;
    tracing::info!(path = %path.display(), "choosing files and opening links for the apps");
    Ok(Served { shared })
}

#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the chooser service accepts and serves on its own threads"
)]
fn accept(listener: &UnixListener, shared: &Arc<Shared>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let shared = Arc::clone(shared);
                if let Err(error) = std::thread::Builder::new()
                    .name("tilo-chooser-app".into())
                    .spawn(move || serve_one(&shared, stream))
                {
                    tracing::warn!(%error, "a chooser connection could not be served");
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
                tracing::warn!(%error, "the chooser service's accept failed");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// One app's connection: who it is, as its launcher named it, and what it holds.
struct Connection {
    shared: Arc<Shared>,
    /// Which connection this is, for its questions' handles.
    number: u64,
    app: String,
    workspace: String,
    writer: Mutex<UnixStream>,
    /// The selected files whose contents the app may still read, by grant token.
    held: Mutex<HashMap<String, Held>>,
    /// One sender per question still up for this app: dropping them, when the app goes, takes the
    /// questions down.
    asking: Mutex<Vec<tokio::sync::oneshot::Sender<()>>>,
}

/// A selected file the app may read the bytes of. The path stays in this session: the app holds
/// only the grant's token, and a folder of ten thousand files leaves no handle open between
/// reads.
struct Held {
    request: u64,
    name: String,
    size: u64,
    path: PathBuf,
}

#[allow(
    clippy::disallowed_methods,
    reason = "session helper: a request that waits for the person or the loop runs on its own thread"
)]
fn serve_one(shared: &Arc<Shared>, stream: UnixStream) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    // The first line is the launcher's, written before the app could write anything.
    let Some(handshake) = read_handshake(&mut reader) else {
        tracing::warn!("a chooser connection named no app: it is closed");
        return;
    };
    let connection = Arc::new(Connection {
        shared: Arc::clone(shared),
        number: shared.connections.fetch_add(1, Ordering::Relaxed),
        app: handshake.app_id,
        workspace: handshake.workspace_id,
        writer: Mutex::new(writer),
        held: Mutex::new(HashMap::new()),
        asking: Mutex::new(Vec::new()),
    });
    loop {
        let frame = match wire_frame::read_line(&mut reader, MAX_FRAME_BYTES) {
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
                connection.held.lock_recover().remove(&grant.token);
                connection.say(&Reply::ContentsReleased { id });
            }
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
    // The app is gone: what it held is let go, and what it was asking taken down.
    connection.held.lock_recover().clear();
    connection.asking.lock_recover().clear();
}

/// What a pick hands the app once the person chose.
#[derive(Clone, Copy)]
enum Handing {
    /// Paths in the app's view of the documents store.
    Paths,
    /// Only the chosen files' bytes, on the connection.
    Contents,
}

/// The question a pick asks in the session's file chooser, and what it hands over; `None` for a
/// request that is no pick.
fn pick_question(request: Request) -> Option<(u64, FileChooserQuestion, Handing)> {
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
            FileChooserQuestion {
                parent_window: None,
                title,
                save: mode == PickMode::Save,
                directory: matches!(mode, PickMode::Directory | PickMode::WritableDirectory),
                writable_directory: mode == PickMode::WritableDirectory,
                multiple,
                accept_label: Some(accept_label).filter(|label| !label.is_empty()),
                current_name: suggested_name.filter(|name| !name.is_empty()),
                filters: filters.iter().map(picker_filter).collect(),
            },
            Handing::Paths,
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
            FileChooserQuestion {
                title,
                multiple: multiple && !directory,
                directory,
                accept_label: Some(accept_label).filter(|label| !label.is_empty()),
                filters: filters.iter().map(picker_filter).collect(),
                ..FileChooserQuestion::default()
            },
            Handing::Contents,
        )),
        _ => None,
    }
}

/// The launcher's handshake, the connection's first line.
fn read_handshake(reader: &mut BufReader<UnixStream>) -> Option<Handshake> {
    let frame = wire_frame::read_line(reader, MAX_FRAME_BYTES).ok()??;
    let handshake: Handshake = serde_json::from_slice(frame.trim_ascii_end()).ok()?;
    let named = |name: &str| !name.is_empty() && name.len() <= MAX_NAME;
    (handshake.protocol == PROTOCOL && named(&handshake.app_id) && named(&handshake.workspace_id))
        .then_some(handshake)
}

impl Connection {
    /// Write `reply` as its line; a reply the wire cannot carry is logged, and an app that stopped
    /// reading has gone.
    fn say(&self, reply: &Reply) {
        let Ok(mut frame) = serde_json::to_vec(reply) else {
            return;
        };
        if frame.len() >= MAX_FRAME_BYTES {
            tracing::warn!(app = %self.app, "a chooser reply is larger than the wire allows");
            return;
        }
        frame.push(b'\n');
        let mut writer = self.writer.lock_recover();
        let _ = tilo_failure::ignored!(
            "an app that closed its end of the chooser asked nothing more: no one is left to answer",
            writer.write_all(&frame).and_then(|()| writer.flush())
        );
    }

    /// Answer a request that waits for the session's loop.
    fn answer(&self, request: Request) -> Reply {
        match request {
            Request::Open { id, path } => match self.opened_path(&path) {
                Ok(host) => match tilo_portal::file_chooser::file_uri(&host) {
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
            | Request::ReadContents { id, .. }
            | Request::ReleaseContents { id, .. }
            | Request::ChooseApp { id, .. }
            | Request::AwaitIdentity { id }
            | Request::ChooseIdentity { id } => failed(id, DOES_NOT_FIT),
        }
    }

    /// Who asks, as the portal's chooser knows an app: in its launcher's sandbox when this session
    /// started it with the documents store bound in.
    fn caller(&self) -> PortalCaller {
        let sandboxed = self
            .shared
            .serving
            .launches
            .sandboxed_apps()
            .contains(&self.app);
        PortalCaller {
            workspace: Some(self.workspace.clone()),
            sandboxed,
            ..PortalCaller::new(self.app.clone())
        }
    }

    /// Ask the person what `request` asks for in the session's file chooser, and answer the app
    /// once they chose. Their answer can take as long as they like, so the question runs as a task
    /// of the session's runtime ([`crate::landing::off_loop`]) and nothing on this connection waits
    /// for it: the app's other requests are answered meanwhile, and the reply is written when the
    /// choice lands.
    fn ask_person(connection: &Arc<Self>, request: Request) {
        let Some((id, question, handing)) = pick_question(request) else {
            return;
        };
        let caller = connection.caller();
        let documents = match handing {
            Handing::Paths => match (&connection.shared.serving.documents, caller.sandboxed) {
                (Some(documents), _) => Some(documents.clone()),
                // An app in a sandbox reads only what the store hands it.
                (None, true) => return connection.say(&failed(id, CANNOT_HAND)),
                (None, false) => Some(tilo_documents::Documents::new()),
            },
            Handing::Contents => None,
        };
        let Some(questions) = connection.shared.serving.questions.clone() else {
            return connection.say(&Reply::Nobody { id });
        };
        let kind = tilo_portal::file_chooser::kind_of(&question);
        let directory = question.directory;
        let handle = format!("chooser:{}:{id}", connection.number);
        let (taken_down, gone) = tokio::sync::oneshot::channel();
        connection.asking.lock_recover().push(taken_down);
        let asking = caller.clone();
        let chosen = crate::landing::off_loop(async move {
            let asked = tilo_portal::file_chooser::choose(&questions, &asking, handle, &question);
            futures_util::pin_mut!(asked);
            match futures_util::future::select(asked, gone).await {
                futures_util::future::Either::Left((chosen, _)) => chosen,
                futures_util::future::Either::Right(_) => Err(Ended::Cancelled),
            }
        });
        let answering = Arc::clone(connection);
        chosen.then(move |chosen| {
            // Handing the files over opens them and writes to the app: not on the runtime's thread.
            let hand = move || {
                answering
                    .asking
                    .lock_recover()
                    .retain(|taken_down| !taken_down.is_closed());
                let reply = match chosen {
                    Some(Ok(paths)) => match documents {
                        Some(documents) => {
                            let exports =
                                tilo_portal::doc_store::exports(&documents, &caller, &paths, kind);
                            Reply::Picked {
                                id,
                                paths: exports.into_iter().map(|export| export.app_path).collect(),
                            }
                        }
                        None => answering.contents(id, &paths, directory),
                    },
                    Some(Err(Ended::Cancelled)) => Reply::Cancelled { id },
                    Some(Err(Ended::Failed)) => failed(id, DOES_NOT_FIT),
                    None => failed(id, NOT_ASKED),
                };
                answering.say(&reply);
            };
            match tilo_rt::try_app() {
                Ok(runtime) => {
                    runtime.spawn_blocking(hand);
                }
                Err(error) => {
                    tracing::warn!(%error, "no thread hands the chosen files over: answered here");
                    hand();
                }
            }
        });
    }

    /// The contents of what the person chose, `paths`: opened here, and only their bytes handed on,
    /// on this connection.
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
            let Ok(file) = tilo_dialogs::portal::open_regular_file(path) else {
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
                let Ok(file) = tilo_dialogs::portal::open_regular_file(&entry.path()) else {
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
        let mut held = self.held.lock_recover();
        if held.len().saturating_add(opened.len()) > MAX_HELD {
            return failed(id, TOO_LARGE);
        }
        let grants = opened
            .into_iter()
            .map(|(name, size, path, _file)| {
                let token = uuid::Uuid::new_v4().to_string();
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

    /// The host file the app asks to open at `path`: one the person chose for it, named by the path
    /// its view of the documents store gives it. An app outside a sandbox reads the host already,
    /// and names the file itself.
    fn opened_path(&self, path: &Path) -> Result<PathBuf, &'static str> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(NOT_CHOSEN);
        }
        let caller = self.caller();
        if !caller.sandboxed {
            return Ok(path.to_path_buf());
        }
        let documents = self.shared.serving.documents.as_ref().ok_or(NOT_CHOSEN)?;
        let key = tilo_portal::doc_store::key_of(&caller).ok_or(NOT_CHOSEN)?;
        let (token, inner) = tilo_documents::split_sandbox_path(path).ok_or(NOT_CHOSEN)?;
        let document = documents.document(&key, &token).ok_or(NOT_CHOSEN)?;
        let within = inner
            .strip_prefix(document.name())
            .map_err(|_| NOT_CHOSEN)?;
        if within.as_os_str().is_empty() {
            return Ok(document.host);
        }
        // Inside a chosen folder, and only a folder.
        if matches!(document.kind, tilo_documents::Kind::Folder { .. }) {
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
            let Some(uri) = tilo_portal::file_chooser::file_uri(&host) else {
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

    /// Ask the session's loop to open `uri` in the app's workspace: whether an app took it.
    fn open(&self, uri: &str) -> Result<bool, &'static str> {
        let (reply, answered) = std::sync::mpsc::sync_channel(1);
        self.shared.opens.lock_recover().push(Open {
            workspace: self.workspace.clone(),
            uri: uri.to_owned(),
            reply,
        });
        (self.shared.serving.wake)();
        match answered.recv_timeout(OPEN_WAIT) {
            Ok(Ok(opened)) => Ok(opened.is_some()),
            Ok(Err(error)) => {
                tracing::warn!(app = %self.app, %error, "what an app asked to open did not open");
                Err(DID_NOT_OPEN)
            }
            Err(_) => Err(NOT_IN_TIME),
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
        let held = connection.held.lock_recover();
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
    let Ok(file) = tilo_dialogs::portal::open_regular_file(&path) else {
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

/// A filter the app named, as the session's file chooser shows one.
fn picker_filter(filter: &Filter) -> PickerFilter {
    let pairs: Vec<(u32, String)> = filter
        .globs
        .iter()
        .map(|glob| (0, glob.clone()))
        .chain(filter.mimetypes.iter().map(|mime| (1, mime.clone())))
        .collect();
    PickerFilter::from_pairs(filter.name.clone(), &pairs)
}

fn failed(id: u64, reason: &str) -> Reply {
    Reply::Failed {
        id,
        reason: reason.to_owned(),
    }
}

#[cfg(test)]
mod tests;
