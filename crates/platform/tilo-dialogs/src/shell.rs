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
//! computer it answers [`Reply::Nobody`] and the app falls back to the
//! unattended picker ([`crate::unattended_picker`]).

use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::os::fd::{FromRawFd as _, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::portal::{
    ContentsChannel, ContentsOutcome, FileFilter, LaunchChannel, LaunchOutcome, LaunchRequest,
    MultiChannel, MultiOutcome, OpenFileChannel, OpenFileOutcome, OpenedFile, PickKind,
    PickRequest, PickedContents, PortalChannel, PortalOutcome,
};
use crate::{FilePicker, answered};

/// The service's name: its socket is `<runtime>/tilo/services/chooser.sock`.
pub const SERVICE: &str = "chooser";
/// Where the launcher tells the app its connection's descriptor number.
pub const FD_ENV: &str = "TILO_SVC_CHOOSER_FD";
/// Version of the launcher's first frame and of [`Request`]/[`Reply`].
pub const PROTOCOL: u16 = 1;
/// Longest frame either side accepts.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;

const NOBODY: &str = "nobody is at this computer to choose a file; name the file instead";
const CLOSED: &str = "the session's file chooser is gone";

/// The launcher's first frame on a new connection: who the app is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handshake {
    pub protocol: u16,
    pub app_id: String,
    /// The room the app was launched into.
    pub workspace_id: String,
}

/// Which chooser a [`Request::Pick`] opens.
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

/// One entry of the chooser's file-type control.
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
pub const MAX_CONTENT_FILES: usize = 16;

/// App → shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    /// Show a chooser; answered by [`Reply::Picked`], [`Reply::Cancelled`],
    /// [`Reply::Nobody`] or [`Reply::Failed`].
    Pick {
        id: u64,
        mode: PickMode,
        #[serde(default)]
        multiple: bool,
        title: String,
        #[serde(default)]
        accept_label: String,
        /// The filename a Save chooser starts with.
        #[serde(default)]
        suggested_name: Option<String>,
        #[serde(default)]
        filters: Vec<Filter>,
    },
    /// Owner selection for contents; the app supplies no filesystem path.
    PickContents {
        id: u64,
        multiple: bool,
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
    /// Open `path` in the app that owns its type; answered by
    /// [`Reply::Opened`] or [`Reply::Failed`].
    Open {
        id: u64,
        path: PathBuf,
    },
}

/// Shell → app. Every reply names the request it answers.
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
    Picked {
        id: u64,
        paths: Vec<PathBuf>,
    },
    Cancelled {
        id: u64,
    },
    /// Nobody is at the computer to choose.
    Nobody {
        id: u64,
    },
    Opened {
        id: u64,
    },
    Failed {
        id: u64,
        reason: String,
    },
}

impl Reply {
    #[must_use]
    pub fn id(&self) -> u64 {
        match self {
            Reply::ContentsGranted { id, .. }
            | Reply::ContentsChunk { id, .. }
            | Reply::ContentsReleased { id }
            | Reply::Picked { id, .. }
            | Reply::Cancelled { id }
            | Reply::Nobody { id }
            | Reply::Opened { id }
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

type Answer = Box<dyn FnOnce(Reply) + Send>;

struct Connection {
    writer: Mutex<UnixStream>,
    /// `None` once the shell hung up: every later request fails at once.
    pending: Mutex<Option<HashMap<u64, Answer>>>,
    next_id: AtomicU64,
}

impl Connection {
    /// Send `request` (its id is filled in) and call `answer` with the reply,
    /// on the connection's reader thread, or with a failure if it cannot be sent.
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
            answer(Reply::Failed {
                id,
                reason: CLOSED.to_owned(),
            });
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
                writer.write_all(&frame).and_then(|()| writer.flush())
            });
        if let Err(error) = sent
            && let Some(answer) = self.take(id)
        {
            answer(Reply::Failed {
                id,
                reason: format!("could not reach the session's file chooser: {error}"),
            });
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
            answer(Reply::Failed {
                id,
                reason: CLOSED.to_owned(),
            });
        }
    }
}

fn read_replies(connection: &Connection, stream: UnixStream) {
    let mut reader = BufReader::new(stream);
    while let Ok(Some(frame)) = wire_frame::read_line(&mut reader, MAX_FRAME_BYTES) {
        let Ok(reply) = serde_json::from_slice::<Reply>(frame.trim_ascii_end()) else {
            tracing::warn!("the session's file chooser sent an unreadable reply");
            break;
        };
        if let Some(answer) = connection.take(reply.id()) {
            answer(reply);
        }
    }
    connection.close();
}

/// The shell's choosers over `stream`, a connection the launcher opened and
/// named. When the shell answers that nobody is at the computer, `unattended`
/// answers instead (a Save into the save directory); without one, the
/// request fails.
///
/// # Errors
/// The connection cannot be duplicated for its reader thread, or the thread
/// cannot start.
pub fn picker(
    stream: UnixStream,
    unattended: Option<Arc<dyn FilePicker>>,
) -> std::io::Result<Arc<dyn FilePicker>> {
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
    Ok(Arc::new(ShellChooser {
        connection,
        unattended,
    }))
}

/// [`picker`] over the connection the launcher handed this process at
/// [`FD_ENV`], or `None` when it was handed none (the session has a desktop
/// portal, or the app runs outside Tilo). The descriptor is adopted once;
/// later calls return the same picker.
pub fn inherited_picker(unattended: Option<Arc<dyn FilePicker>>) -> Option<Arc<dyn FilePicker>> {
    static INHERITED: OnceLock<Option<Arc<dyn FilePicker>>> = OnceLock::new();
    INHERITED
        .get_or_init(|| {
            let fd = std::env::var(FD_ENV).ok()?.parse::<RawFd>().ok()?;
            let stream = adopt_socket(fd)?;
            picker(stream, unattended)
                .inspect_err(
                    |error| tracing::warn!(%error, "the session's file chooser is unusable"),
                )
                .ok()
        })
        .clone()
}

/// Take ownership of `fd` if it is an open socket. A stale variable naming
/// a number this process uses for something else is left alone.
fn adopt_socket(fd: RawFd) -> Option<UnixStream> {
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
    Some(unsafe { UnixStream::from_raw_fd(fd) })
}

struct ShellChooser {
    connection: Arc<Connection>,
    unattended: Option<Arc<dyn FilePicker>>,
}

/// A channel whose one outcome wakes the event loop that asked.
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
    /// Ask the shell, answering from the unattended picker when nobody is
    /// there to choose.
    fn pick_with<T: Send + 'static>(
        &self,
        request: PickRequest,
        deliver: impl FnOnce(T) + Send + 'static,
        on_paths: impl FnOnce(Vec<PathBuf>) -> T + Send + 'static,
        cancelled: T,
        failed: impl FnOnce(String) -> T + Send + 'static,
        unattended: impl FnOnce(&dyn FilePicker, PickRequest) -> T + Send + 'static,
    ) {
        let fallback = self.unattended.clone();
        let build = pick_request(&request);
        self.connection.ask(
            build,
            Box::new(move |reply| {
                deliver(match reply {
                    Reply::Picked { paths, .. } if !paths.is_empty() => on_paths(paths),
                    Reply::Picked { .. } | Reply::Cancelled { .. } => cancelled,
                    Reply::Nobody { .. } => match fallback {
                        Some(picker) => unattended(&*picker, request),
                        None => failed(NOBODY.to_owned()),
                    },
                    Reply::Failed { reason, .. } => failed(reason),
                    _ => failed("the session's file chooser answered out of turn".to_owned()),
                });
            }),
        );
    }
}

fn closed_recv<T>(rx: &std::sync::mpsc::Receiver<T>, closed: T) -> T {
    rx.recv().unwrap_or(closed)
}

impl Drop for ShellChooser {
    fn drop(&mut self) {
        if let Ok(writer) = self.connection.writer.lock() {
            let _ = writer.shutdown(std::net::Shutdown::Both);
        }
        self.connection.close();
    }
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
                    Box::new(move |reply| {
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
                location: None,
            });
        }
        Ok(files)
    })();
    for grant in grants {
        connection.ask(
            move |id| Request::ReleaseContents { id, grant },
            Box::new(|_| {}),
        );
    }
    result.map_or_else(ContentsOutcome::Failed, ContentsOutcome::Picked)
}

impl FilePicker for ShellChooser {
    fn pick_contents(&self, request: PickRequest) -> ContentsChannel {
        if !request.is_open() {
            return answered(ContentsOutcome::Failed(
                "only an Open chooser delivers contents".to_owned(),
            ));
        }
        let (deliver, rx) = channel();
        let connection = Arc::clone(&self.connection);
        let fallback = self.unattended.clone();
        let title = request.title.clone();
        let accept_label = request.accept_label.clone();
        let multiple = request.is_multiple();
        let filters = request.filters.iter().map(Filter::from_portal).collect();
        self.connection.ask(
            move |id| Request::PickContents {
                id,
                multiple,
                title,
                accept_label,
                filters,
            },
            Box::new(move |reply| {
                let spawned = tilo_rt::background::Background::current().spawn(
                    "tilo-portal-contents",
                    move || {
                        let outcome = match reply {
                            Reply::ContentsGranted { grants, .. } => {
                                read_contents(&connection, grants)
                            }
                            Reply::Cancelled { .. } => ContentsOutcome::Cancelled,
                            Reply::Nobody { .. } => fallback.map_or_else(
                                || ContentsOutcome::Failed(NOBODY.to_owned()),
                                |picker| {
                                    closed_recv(
                                        &picker.pick_contents(request),
                                        ContentsOutcome::Failed(CLOSED.to_owned()),
                                    )
                                },
                            ),
                            Reply::Failed { reason, .. } => ContentsOutcome::Failed(reason),
                            _ => ContentsOutcome::Failed(
                                "the session's file chooser answered out of turn".to_owned(),
                            ),
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
            |paths| {
                paths
                    .into_iter()
                    .next()
                    .map_or(PortalOutcome::Cancelled, PortalOutcome::Opened)
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
            MultiOutcome::Selected,
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
        let (deliver, rx) = channel();
        self.pick_with(
            request,
            deliver,
            |paths| match paths.into_iter().next() {
                Some(path) => match crate::portal::open_regular_file(&path) {
                    Ok(file) => OpenFileOutcome::Opened(OpenedFile::new(
                        file,
                        path.file_name().map_or_else(
                            || "selected file".to_owned(),
                            |name| name.to_string_lossy().into_owned(),
                        ),
                    )),
                    Err(error) => {
                        OpenFileOutcome::Failed(format!("could not open selected file: {error}"))
                    }
                },
                None => OpenFileOutcome::Cancelled,
            },
            OpenFileOutcome::Cancelled,
            OpenFileOutcome::Failed,
            |picker, request| {
                closed_recv(
                    &picker.open_file(request),
                    OpenFileOutcome::Failed(CLOSED.into()),
                )
            },
        );
        rx
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
        self.connection.ask(
            move |id| Request::Open { id, path },
            Box::new(move |reply| {
                deliver(match reply {
                    Reply::Opened { .. } => LaunchOutcome::Opened,
                    Reply::Failed { reason, .. } => LaunchOutcome::Failed(reason),
                    _ => LaunchOutcome::Failed(
                        "the session's file chooser answered out of turn".into(),
                    ),
                });
            }),
        );
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead as _, Read as _};
    use std::path::Path;
    use std::time::Duration;

    const ANSWER: Duration = Duration::from_secs(10);

    /// A stand-in shell: reads each request and answers with `respond`.
    fn shell(respond: impl Fn(Request) -> Reply + Send + 'static) -> UnixStream {
        let (app, shell) = UnixStream::pair().unwrap();
        std::thread::spawn(move || {
            let mut writer = shell.try_clone().unwrap();
            for line in BufReader::new(shell).lines() {
                let Ok(line) = line else { break };
                let request: Request = serde_json::from_str(&line).unwrap();
                let mut frame = serde_json::to_vec(&respond(request)).unwrap();
                frame.push(b'\n');
                if writer.write_all(&frame).is_err() {
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
            | Request::PickContents { id, .. }
            | Request::ReadContents { id, .. }
            | Request::ReleaseContents { id, .. } => *id,
        }
    }

    #[test]
    fn only_an_inherited_socket_is_adopted() {
        use std::os::fd::IntoRawFd as _;

        let file = tempfile::tempfile().unwrap().into_raw_fd();
        assert!(adopt_socket(file).is_none(), "a file is not the chooser");
        // SAFETY: the test owns `file` and nothing adopted it.
        drop(unsafe { std::fs::File::from_raw_fd(file) });
        assert!(adopt_socket(2).is_none());

        let (app, mut shell) = UnixStream::pair().unwrap();
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

    #[test]
    fn the_shell_chooser_keeps_the_picker_contract() {
        let chooser = picker(
            shell(|request| Reply::Failed {
                id: id_of(&request),
                reason: "no".into(),
            }),
            None,
        )
        .unwrap();
        crate::conformance::check_picker_contract(&*chooser);
    }

    #[test]
    fn a_pick_carries_the_request_and_delivers_what_the_shell_chose() {
        let (seen_tx, seen) = std::sync::mpsc::channel();
        let chooser = picker(
            shell(move |request| {
                let id = id_of(&request);
                let _ = seen_tx.send(request);
                Reply::Picked {
                    id,
                    paths: vec!["/data/in/x.xlsx".into(), "/data/in/y.xlsx".into()],
                }
            }),
            None,
        )
        .unwrap();
        let request = PickRequest::open("Import spreadsheet", "Import")
            .filter(FileFilter::new("Excel").glob("*.xlsx"));
        assert_eq!(
            chooser.pick(request).recv_timeout(ANSWER).unwrap(),
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
        } = seen.recv_timeout(ANSWER).unwrap()
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
                .recv_timeout(ANSWER)
                .unwrap(),
            MultiOutcome::Selected(vec!["/data/in/x.xlsx".into(), "/data/in/y.xlsx".into()])
        );
        let Request::Pick { multiple, .. } = seen.recv_timeout(ANSWER).unwrap() else {
            panic!("a chooser request is a pick");
        };
        assert!(multiple);
    }

    #[test]
    fn nobody_at_the_computer_falls_back_to_the_unattended_picker() {
        let directory = tempfile::tempdir().unwrap();
        let unattended =
            crate::unattended_picker(directory.path().to_path_buf(), crate::unavailable_picker());
        let nobody = |request: Request| Reply::Nobody {
            id: id_of(&request),
        };
        let chooser = picker(shell(nobody), unattended).unwrap();
        assert_eq!(
            chooser
                .pick(PickRequest::save("Save", "Save", "report.xlsx"))
                .recv_timeout(ANSWER)
                .unwrap(),
            PortalOutcome::Opened(directory.path().join("report.xlsx"))
        );
        assert!(matches!(
            chooser.pick(PickRequest::open("Open", "Open")).recv_timeout(ANSWER),
            Ok(PortalOutcome::Failed(reason)) if reason.contains("nobody")
        ));

        let alone = picker(shell(nobody), None).unwrap();
        assert!(matches!(
            alone
                .pick(PickRequest::save("Save", "Save", "report.xlsx"))
                .recv_timeout(ANSWER),
            Ok(PortalOutcome::Failed(reason)) if reason.contains("nobody")
        ));
    }

    #[test]
    fn opening_a_file_asks_the_shell_and_reports_its_answer() {
        let chooser = picker(
            shell(|request| match request {
                Request::Open { id, path } if path == Path::new("/data/in/a.docx") => {
                    Reply::Opened { id }
                }
                other => Reply::Failed {
                    id: id_of(&other),
                    reason: "not granted".into(),
                },
            }),
            None,
        )
        .unwrap();
        assert_eq!(
            chooser
                .launch(LaunchRequest::open("/data/in/a.docx"))
                .recv_timeout(ANSWER)
                .unwrap(),
            LaunchOutcome::Opened
        );
        assert_eq!(
            chooser
                .launch(LaunchRequest::open("/etc/shadow"))
                .recv_timeout(ANSWER)
                .unwrap(),
            LaunchOutcome::Failed("not granted".into())
        );
    }

    #[test]
    fn a_cancelled_pick_and_a_gone_shell_answer_instead_of_hanging() {
        let chooser = picker(
            shell(|request| Reply::Cancelled {
                id: id_of(&request),
            }),
            None,
        )
        .unwrap();
        assert_eq!(
            chooser
                .pick(PickRequest::open("Open", "Open"))
                .recv_timeout(ANSWER)
                .unwrap(),
            PortalOutcome::Cancelled
        );

        let (app, shell) = UnixStream::pair().unwrap();
        let chooser = picker(app, None).unwrap();
        let waiting = chooser.pick(PickRequest::open("Open", "Open"));
        drop(shell);
        assert!(matches!(
            waiting.recv_timeout(ANSWER),
            Ok(PortalOutcome::Failed(_))
        ));
        assert!(matches!(
            chooser
                .launch(LaunchRequest::open("/data/in/a.docx"))
                .recv_timeout(ANSWER),
            Ok(LaunchOutcome::Failed(_))
        ));
    }
}
