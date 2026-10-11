//! The shell's own file choosers, for apps that reach no desktop portal (a
//! provider's headless guest runs no session bus).
//!
//! The launcher connects every app launched into a room and names the app
//! and the room before the sandbox starts (`tilo_dialogs::shell`), so a
//! request is attributed without trusting the app. A chooser is the shell's
//! file picker, on screen, limited to the paths the app's workspace grants
//! it; whatever comes back is checked against the grants again before the
//! app hears it. Opening a file hands it to the compositor thread, which
//! opens it in the app that owns its type.
//!
//! Who answers a chooser: a session with someone at it (an attached agent
//! driving the screen, or an ordinary desktop) shows it; an unattended one
//! answers [`Reply::Nobody`] and the app falls back to its unattended picker.

use std::io::{BufRead, BufReader, Read as _, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use collab_types::caps::Verb;
use tilo_dialogs::shell::{Handshake, MAX_FRAME_BYTES, PROTOCOL, PickMode, Reply, Request};

use crate::portal::{
    FilePickerRequest, FilePickerSpawner, GrantStoreHandle, PickerFilter, PickerScope, PortalCaller,
};
use crate::shell::i18n::{t, t_args};

/// Whether someone is at the computer to answer a chooser.
pub type Presence = Arc<dyn Fn() -> bool + Send + Sync>;

/// What the service needs from the session.
pub struct ChooserContext {
    pub grants: GrantStoreHandle,
    pub pickers: Arc<dyn FilePickerSpawner>,
    pub presence: Presence,
    /// Where an unattended session saves; a Save chooser starts there.
    pub save_directory: Option<PathBuf>,
    /// Wakes the compositor loop to mount a queued chooser or run an open.
    pub wake: crate::session_runtime::Wake,
}

/// An app asked for `path` to be opened in the app that owns its type.
/// Answered on the compositor thread, which owns the tree and the launcher.
pub struct OpenRequest {
    pub caller: PortalCaller,
    pub path: PathBuf,
    pub reply: mpsc::Sender<Result<(), String>>,
}

pub struct ChooserService {
    grants: GrantStoreHandle,
    opens: mpsc::Receiver<OpenRequest>,
    stop: Arc<AtomicBool>,
    acceptor: Option<std::thread::JoinHandle<()>>,
    path: PathBuf,
}

impl ChooserService {
    /// Serve choosers at the launcher's chooser socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket or its accept thread cannot be created.
    pub fn start(context: ChooserContext) -> Result<Self, String> {
        launcher::services::ensure_services_root()
            .map_err(|error| format!("create chooser service directory: {error}"))?;
        Self::start_at(
            launcher::services::socket_for(launcher::services::CHOOSER_SERVICE),
            context,
        )
    }

    fn start_at(path: PathBuf, context: ChooserContext) -> Result<Self, String> {
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove stale chooser socket: {error}"))?;
        }
        let listener = UnixListener::bind(&path)
            .map_err(|error| format!("bind chooser service {}: {error}", path.display()))?;
        let (opens_tx, opens_rx) = mpsc::channel();
        let grants = context.grants.clone();
        let context = Arc::new(context);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let acceptor = std::thread::Builder::new()
            .name("tilo-chooser-accept".into())
            .spawn(move || {
                loop {
                    match listener.accept() {
                        Ok(_) if thread_stop.load(Ordering::Acquire) => break,
                        Ok((stream, _)) => {
                            let context = Arc::clone(&context);
                            let opens = opens_tx.clone();
                            let _ = std::thread::Builder::new()
                                .name("tilo-chooser-conn".into())
                                .spawn(move || serve_connection(stream, &context, &opens));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            if thread_stop.load(Ordering::Acquire) {
                                break;
                            }
                            tracing::warn!(%error, "chooser service accept failed");
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            })
            .map_err(|error| {
                let _ = std::fs::remove_file(&path);
                format!("spawn chooser service: {error}")
            })?;
        Ok(Self {
            grants,
            opens: opens_rx,
            stop,
            acceptor: Some(acceptor),
            path,
        })
    }

    /// Opens still permitted at dispatch; each returned request must be answered.
    pub fn drain_opens(&self) -> Vec<OpenRequest> {
        self.opens
            .try_iter()
            .filter_map(|mut open| {
                // Admission can precede compositor dispatch by several frames.
                // Resolve the path again against the original app's current
                // workspace policy rather than handing over stale approval.
                match permitted(
                    &self.grants,
                    &open.caller,
                    std::slice::from_ref(&open.path),
                    PickMode::Open,
                ) {
                    Ok(mut paths) => {
                        open.path = paths.remove(0);
                        Some(open)
                    }
                    Err(reason) => {
                        let _ = open.reply.send(Err(reason));
                        None
                    }
                }
            })
            .collect()
    }
}

impl Drop for ChooserService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // The acceptor sleeps in `accept`; one connection wakes it to see the
        // flag. If nothing can connect it is not waited for.
        if let Some(acceptor) = self.acceptor.take()
            && UnixStream::connect(&self.path).is_ok()
        {
            let _ = acceptor.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

fn serve_connection(
    stream: UnixStream,
    context: &Arc<ChooserContext>,
    opens: &mpsc::Sender<OpenRequest>,
) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let writer = Arc::new(Mutex::new(writer));
    let mut reader = BufReader::new(stream);
    let Some(caller) = read_frame(&mut reader)
        .ok()
        .flatten()
        .and_then(|frame| serde_json::from_slice::<Handshake>(&frame).ok())
        .and_then(|handshake| authenticate(&context.grants, &handshake))
    else {
        return;
    };
    loop {
        let request = match read_frame(&mut reader) {
            Ok(Some(frame)) => match serde_json::from_slice::<Request>(&frame) {
                Ok(request) => request,
                Err(error) => {
                    tracing::warn!(%error, app = %caller.app_id, "chooser request rejected");
                    break;
                }
            },
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, app = %caller.app_id, "chooser frame rejected");
                break;
            }
        };
        // A chooser waits on a person; the connection keeps reading so a
        // second request is not queued behind it.
        let context = Arc::clone(context);
        let opens = opens.clone();
        let caller = caller.clone();
        let writer = Arc::clone(&writer);
        let _ = std::thread::Builder::new()
            .name("tilo-chooser-req".into())
            .spawn(move || {
                let reply = answer(&context, &opens, &caller, request);
                send(&writer, &reply);
            });
    }
}

/// The caller the launcher named, if its room is one of this session's.
fn authenticate(grants: &GrantStoreHandle, handshake: &Handshake) -> Option<PortalCaller> {
    if handshake.protocol != PROTOCOL
        || handshake.app_id.trim().is_empty()
        || handshake.app_id.len() > 255
    {
        return None;
    }
    grants.caller_in_room(&handshake.workspace_id, &handshake.app_id)
}

fn answer(
    context: &ChooserContext,
    opens: &mpsc::Sender<OpenRequest>,
    caller: &PortalCaller,
    request: Request,
) -> Reply {
    match request {
        Request::Pick {
            id,
            mode,
            multiple,
            title,
            accept_label,
            suggested_name,
            filters,
        } => {
            if !(context.presence)() {
                return Reply::Nobody { id };
            }
            let Some(scope) = authorized_scope(context, caller, mode) else {
                return Reply::Failed {
                    id,
                    reason: t("portal-chooser-nothing-granted"),
                };
            };
            let (reply, chosen) = tokio::sync::oneshot::channel();
            context.pickers.spawn(FilePickerRequest {
                caller: caller.clone(),
                title,
                save: mode == PickMode::Save,
                directory: mode == PickMode::Directory,
                multiple,
                accept_label: Some(accept_label).filter(|label| !label.trim().is_empty()),
                current_name: suggested_name,
                filters: filters
                    .iter()
                    .map(|filter| {
                        let pairs = filter
                            .globs
                            .iter()
                            .map(|glob| (0, glob.clone()))
                            .chain(filter.mimetypes.iter().map(|mime| (1, mime.clone())))
                            .collect::<Vec<_>>();
                        PickerFilter::from_pairs(filter.name.clone(), &pairs)
                    })
                    .collect(),
                scope: Some(scope),
                reply,
            });
            (context.wake)();
            match chosen.blocking_recv() {
                Ok(Some(paths)) if !paths.is_empty() => {
                    match permitted(&context.grants, caller, &paths, mode) {
                        Ok(paths) => Reply::Picked { id, paths },
                        Err(reason) => Reply::Failed { id, reason },
                    }
                }
                _ => Reply::Cancelled { id },
            }
        }
        Request::Open { id, path } => {
            let paths = match permitted(&context.grants, caller, &[path], PickMode::Open) {
                Ok(paths) => paths,
                Err(reason) => return Reply::Failed { id, reason },
            };
            let Some(path) = paths.into_iter().next() else {
                return Reply::Failed {
                    id,
                    reason: t("portal-chooser-no-file"),
                };
            };
            let (reply, outcome) = mpsc::channel();
            if opens
                .send(OpenRequest {
                    caller: caller.clone(),
                    path,
                    reply,
                })
                .is_err()
            {
                return Reply::Failed {
                    id,
                    reason: t("portal-chooser-session-closing"),
                };
            }
            (context.wake)();
            match outcome.recv() {
                Ok(Ok(())) => Reply::Opened { id },
                Ok(Err(reason)) => Reply::Failed { id, reason },
                Err(_) => Reply::Failed {
                    id,
                    reason: t("portal-chooser-session-closing"),
                },
            }
        }
    }
}

/// Derive picker reach from the requested operation's current authority.
fn authorized_scope(
    context: &ChooserContext,
    caller: &PortalCaller,
    mode: PickMode,
) -> Option<PickerScope> {
    let verb = if mode == PickMode::Save {
        Verb::Write
    } else {
        Verb::Read
    };
    let granted = context
        .grants
        .granted_paths(caller)
        .into_iter()
        .filter(|path| context.grants.filesystem_path_allows(caller, path, verb))
        .collect();
    scope_for(granted, context.save_directory.as_deref(), mode)
}

/// The chooser's reach: the directories the app was granted (a granted file
/// shows its folder), starting where the request is most likely to be
/// answered. `None` when the app was granted nothing to choose from.
fn scope_for(
    granted: Vec<PathBuf>,
    save_directory: Option<&Path>,
    mode: PickMode,
) -> Option<PickerScope> {
    let scope = PickerScope {
        initial_dir: PathBuf::new(),
        roots: Vec::new(),
        home: None,
    }
    .with_granted_paths(granted);
    let first = scope.roots.first()?.clone();
    let initial_dir = match (mode, save_directory) {
        (PickMode::Save, Some(directory))
            if scope.roots.iter().any(|root| directory.starts_with(root)) =>
        {
            directory.to_path_buf()
        }
        _ => first,
    };
    Some(PickerScope {
        initial_dir,
        ..scope
    })
}

/// `paths` resolved (symlinks and `..` followed) and each covered by the
/// caller's grants: reading for Open, replacing for Save. A Save target need
/// not exist yet; its folder must.
fn permitted(
    grants: &GrantStoreHandle,
    caller: &PortalCaller,
    paths: &[PathBuf],
    mode: PickMode,
) -> Result<Vec<PathBuf>, String> {
    paths
        .iter()
        .map(|path| {
            let resolved = match mode {
                PickMode::Save => {
                    let name = path
                        .file_name()
                        .filter(|name| Path::new(name).components().count() == 1);
                    let folder = path.parent().and_then(|folder| folder.canonicalize().ok());
                    folder.zip(name).map(|(folder, name)| folder.join(name))
                }
                PickMode::Open | PickMode::Directory => path.canonicalize().ok(),
            };
            let verb = if mode == PickMode::Save {
                Verb::Write
            } else {
                Verb::Read
            };
            resolved
                .filter(|resolved| grants.filesystem_path_allows(caller, resolved, verb))
                .ok_or_else(|| {
                    t_args(
                        "portal-chooser-file-not-granted",
                        &[("path", path.display().to_string().into())],
                    )
                })
        })
        .collect()
}

fn send(writer: &Mutex<UnixStream>, reply: &Reply) {
    let Ok(mut frame) = serde_json::to_vec(reply) else {
        return;
    };
    frame.push(b'\n');
    if let Ok(mut writer) = writer.lock() {
        let _ = writer.write_all(&frame).and_then(|()| writer.flush());
    }
}

fn read_frame(reader: &mut BufReader<UnixStream>) -> std::io::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    let limit = u64::try_from(MAX_FRAME_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let count = reader.by_ref().take(limit).read_until(b'\n', &mut frame)?;
    if count == 0 {
        return Ok(None);
    }
    if frame.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "chooser frame is too long or unterminated",
        ));
    }
    frame.pop();
    Ok(Some(frame))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::FilePickerQueue;
    use crate::shell::state::{Capability, FsMode, GRANT_SESSION, Workspace, WorkspaceId};
    use std::time::Instant;

    const APP: &str = "so.tilo.sheets";

    struct Session {
        service: ChooserService,
        pickers: FilePickerQueue,
        present: Arc<AtomicBool>,
        wakes: Arc<std::sync::atomic::AtomicU64>,
        room: String,
        socket: PathBuf,
        inbox: PathBuf,
        outbox: PathBuf,
        elsewhere: PathBuf,
        _dir: tempfile::TempDir,
    }

    /// A room whose Sheets may read `in/` and write `out/`, and a folder
    /// beside them it was given nothing in.
    fn session_with_save_hint(hint: bool) -> Session {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let (inbox, outbox, elsewhere) = (base.join("in"), base.join("out"), base.join("home"));
        for folder in [&inbox, &outbox, &elsewhere] {
            std::fs::create_dir(folder).unwrap();
        }
        std::fs::write(inbox.join("x.xlsx"), b"sheet").unwrap();
        std::fs::write(elsewhere.join("secret.xlsx"), b"no").unwrap();

        let mut workspace = Workspace::new(WorkspaceId(7), "guest");
        for (path, mode) in [(&inbox, FsMode::Read), (&outbox, FsMode::ReadWrite)] {
            workspace.grant_app(
                APP,
                &Capability::Filesystem {
                    path: (*path).clone(),
                    mode,
                    scope: GRANT_SESSION,
                },
            );
        }
        let room = workspace.room.to_string();
        let (grants, _) = GrantStoreHandle::test_with_workspace(workspace);
        let pickers = FilePickerQueue::new();
        let present = Arc::new(AtomicBool::new(true));
        let presence = {
            let present = Arc::clone(&present);
            Arc::new(move || present.load(Ordering::Acquire))
        };
        let wakes = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let wake_counter = Arc::clone(&wakes);
        let socket = base.join("chooser.sock");
        let service = ChooserService::start_at(
            socket.clone(),
            ChooserContext {
                grants,
                pickers: Arc::new(pickers.clone()),
                presence,
                save_directory: hint.then(|| outbox.clone()),
                wake: Arc::new(move || {
                    wake_counter.fetch_add(1, Ordering::Release);
                }),
            },
        )
        .unwrap();
        Session {
            service,
            pickers,
            present,
            wakes,
            room,
            socket,
            inbox,
            outbox,
            elsewhere,
            _dir: dir,
        }
    }

    struct App {
        writer: UnixStream,
        reader: BufReader<UnixStream>,
    }

    impl App {
        /// Connect as the launcher does, naming `app_id` in `room`.
        fn launch(session: &Session, app_id: &str, room: &str) -> Self {
            let mut writer = UnixStream::connect(&session.socket).unwrap();
            writer
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            writer
                .write_all(&launcher::services::chooser_handshake(app_id, room).unwrap())
                .unwrap();
            let reader = BufReader::new(writer.try_clone().unwrap());
            Self { writer, reader }
        }

        fn send(&mut self, request: &Request) {
            let mut frame = serde_json::to_vec(request).unwrap();
            frame.push(b'\n');
            // A hung-up shell is seen as the missing reply.
            let _ = self.writer.write_all(&frame);
        }

        fn reply(&mut self) -> Option<Reply> {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) | Err(_) => None,
                Ok(_) => Some(serde_json::from_str(&line).unwrap()),
            }
        }
    }

    fn pick(id: u64, mode: PickMode) -> Request {
        Request::Pick {
            id,
            mode,
            multiple: false,
            title: "Import spreadsheet".into(),
            accept_label: "Import".into(),
            suggested_name: (mode == PickMode::Save).then(|| "copy.xlsx".into()),
            filters: vec![tilo_dialogs::shell::Filter {
                name: "Spreadsheets".into(),
                globs: vec!["*.xlsx".into()],
                mimetypes: Vec::new(),
            }],
        }
    }

    /// The chooser the service put on screen, once it has.
    fn shown(session: &Session) -> FilePickerRequest {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("ten seconds fit in an Instant");
        loop {
            if let Some(request) = session.pickers.drain().pop() {
                return request;
            }
            assert!(Instant::now() < deadline, "no chooser was shown");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn open_request(session: &Session) -> OpenRequest {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .expect("ten seconds fit in an Instant");
        loop {
            if let Some(request) = session.service.drain_opens().pop() {
                return request;
            }
            assert!(Instant::now() < deadline, "no open reached the compositor");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn the_launcher_and_the_app_speak_the_same_chooser_protocol() {
        assert_eq!(
            launcher::services::CHOOSER_SERVICE,
            tilo_dialogs::shell::SERVICE
        );
        assert_eq!(
            launcher::sandbox::svc_env_var(launcher::services::CHOOSER_SERVICE),
            tilo_dialogs::shell::FD_ENV
        );
        let frame = launcher::services::chooser_handshake(APP, "room-7").unwrap();
        assert_eq!(frame.last(), Some(&b'\n'));
        assert_eq!(
            serde_json::from_slice::<Handshake>(&frame).unwrap(),
            Handshake {
                protocol: PROTOCOL,
                app_id: APP.into(),
                workspace_id: "room-7".into(),
            }
        );
    }

    #[test]
    fn save_without_a_directory_hint_uses_only_write_authorized_roots() {
        let session = session_with_save_hint(false);
        let mut app = App::launch(&session, APP, &session.room);
        app.send(&pick(1, PickMode::Save));
        let request = shown(&session);
        let scope = request.scope.as_ref().unwrap();
        assert_eq!(scope.initial_dir, session.outbox);
        assert_eq!(scope.roots, vec![session.outbox.clone()]);
        request
            .reply
            .send(Some(vec![session.outbox.join("copy.xlsx")]))
            .unwrap();
        assert_eq!(
            app.reply(),
            Some(Reply::Picked {
                id: 1,
                paths: vec![session.outbox.join("copy.xlsx")],
            })
        );
        // Final authorization remains authoritative, even if a picker were
        // to return a source path outside the write-scoped roots.
        app.send(&pick(2, PickMode::Save));
        shown(&session)
            .reply
            .send(Some(vec![session.inbox.join("x.xlsx")]))
            .unwrap();
        assert!(matches!(app.reply(), Some(Reply::Failed { id: 2, .. })));
        assert_eq!(
            std::fs::read(session.inbox.join("x.xlsx")).unwrap(),
            b"sheet"
        );
    }

    #[test]
    fn a_chooser_shows_only_granted_files_and_returns_only_granted_files() {
        let session = session_with_save_hint(true);
        let mut app = App::launch(&session, APP, &session.room);

        // Phase `open_starts_in_the_grants_and_returns_the_chosen_file`.
        app.send(&pick(1, PickMode::Open));
        let shown_open = shown(&session);
        assert_eq!(shown_open.caller.app_id.as_str(), APP);
        assert!(!shown_open.save);
        assert_eq!(shown_open.accept_label.as_deref(), Some("Import"));
        assert_eq!(shown_open.filters[0].extensions, vec!["xlsx".to_string()]);
        let scope = shown_open
            .scope
            .clone()
            .expect("the service scopes its chooser");
        assert_eq!(
            scope.roots,
            vec![session.inbox.clone(), session.outbox.clone()]
        );
        assert_eq!(scope.initial_dir, session.inbox);
        shown_open
            .reply
            .send(Some(vec![session.inbox.join("../in/x.xlsx")]))
            .unwrap();
        assert_eq!(
            app.reply(),
            Some(Reply::Picked {
                id: 1,
                paths: vec![session.inbox.join("x.xlsx")],
            })
        );

        // Phase `a_file_outside_the_grants_is_never_handed_over`.
        app.send(&pick(2, PickMode::Open));
        shown(&session)
            .reply
            .send(Some(vec![session.elsewhere.join("secret.xlsx")]))
            .unwrap();
        assert!(matches!(app.reply(), Some(Reply::Failed { id: 2, .. })));

        // Phase `save_starts_in_the_save_directory_and_needs_a_write_grant`.
        app.send(&pick(3, PickMode::Save));
        let shown_save = shown(&session);
        assert!(shown_save.save);
        assert_eq!(shown_save.current_name.as_deref(), Some("copy.xlsx"));
        assert_eq!(
            shown_save.scope.clone().unwrap().initial_dir,
            session.outbox
        );
        shown_save
            .reply
            .send(Some(vec![session.outbox.join("copy.xlsx")]))
            .unwrap();
        assert_eq!(
            app.reply(),
            Some(Reply::Picked {
                id: 3,
                paths: vec![session.outbox.join("copy.xlsx")],
            })
        );
        app.send(&pick(4, PickMode::Save));
        shown(&session)
            .reply
            .send(Some(vec![session.inbox.join("copy.xlsx")]))
            .unwrap();
        assert!(
            matches!(app.reply(), Some(Reply::Failed { id: 4, .. })),
            "a read grant does not let the app replace files"
        );

        // Phase `a_closed_chooser_is_a_cancel`.
        app.send(&pick(5, PickMode::Open));
        shown(&session).reply.send(None).unwrap();
        assert_eq!(app.reply(), Some(Reply::Cancelled { id: 5 }));

        // Phase `nobody_at_the_computer_shows_nothing`.
        session.present.store(false, Ordering::Release);
        app.send(&pick(6, PickMode::Open));
        assert_eq!(app.reply(), Some(Reply::Nobody { id: 6 }));
        assert!(session.pickers.is_empty());
    }

    #[test]
    fn an_app_given_nothing_gets_no_chooser() {
        let session = session_with_save_hint(true);
        let mut app = App::launch(&session, "so.tilo.notes", &session.room);
        app.send(&pick(1, PickMode::Open));
        assert!(matches!(app.reply(), Some(Reply::Failed { id: 1, .. })));
        assert!(session.pickers.is_empty());
    }

    #[test]
    fn a_queued_open_rechecks_the_file_before_handing_it_to_the_compositor() {
        let session = session_with_save_hint(true);
        let mut app = App::launch(&session, APP, &session.room);
        let path = session.inbox.join("x.xlsx");
        app.send(&Request::Open {
            id: 1,
            path: path.clone(),
        });
        let deadline = Instant::now().checked_add(Duration::from_secs(10)).unwrap();
        while session.wakes.load(Ordering::Acquire) == 0 {
            assert!(Instant::now() < deadline, "the open was never queued");
            std::thread::sleep(Duration::from_millis(5));
        }
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(session.elsewhere.join("secret.xlsx"), &path).unwrap();
        assert!(
            session.service.drain_opens().is_empty(),
            "an admitted path now outside the grant must not reach the compositor"
        );
        assert!(matches!(app.reply(), Some(Reply::Failed { id: 1, .. })));
    }

    #[test]
    fn opening_a_granted_file_reaches_the_compositor_and_nothing_else_does() {
        let session = session_with_save_hint(true);
        let mut app = App::launch(&session, APP, &session.room);

        app.send(&Request::Open {
            id: 1,
            path: session.inbox.join("x.xlsx"),
        });
        let open = open_request(&session);
        assert_eq!(open.caller.app_id.as_str(), APP);
        assert_eq!(open.caller.workspace_id, WorkspaceId(7));
        assert_eq!(open.path, session.inbox.join("x.xlsx"));
        open.reply.send(Ok(())).unwrap();
        assert_eq!(app.reply(), Some(Reply::Opened { id: 1 }));

        app.send(&Request::Open {
            id: 2,
            path: session.inbox.join("x.xlsx"),
        });
        open_request(&session)
            .reply
            .send(Err("no app opens this".into()))
            .unwrap();
        assert_eq!(
            app.reply(),
            Some(Reply::Failed {
                id: 2,
                reason: "no app opens this".into(),
            })
        );

        app.send(&Request::Open {
            id: 3,
            path: session.elsewhere.join("secret.xlsx"),
        });
        assert!(matches!(app.reply(), Some(Reply::Failed { id: 3, .. })));
        assert!(session.service.drain_opens().is_empty());
    }

    #[test]
    fn a_launch_into_a_room_this_session_lacks_is_hung_up_on() {
        let session = session_with_save_hint(true);
        let mut app = App::launch(&session, APP, "not-a-room");
        app.send(&pick(1, PickMode::Open));
        assert_eq!(app.reply(), None);
        assert!(session.pickers.is_empty());
    }
}
