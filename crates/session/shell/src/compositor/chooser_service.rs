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

mod contents;

use std::io::{BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use collab_types::caps::Access;
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
    let Some(caller) = wire_frame::read_line(&mut reader, MAX_FRAME_BYTES)
        .ok()
        .flatten()
        .and_then(|frame| serde_json::from_slice::<Handshake>(&frame).ok())
        .and_then(|handshake| authenticate(&context.grants, &handshake))
    else {
        return;
    };
    let selections = Arc::new(contents::ConnectionGrants::default());
    if selections.maintain().is_err() {
        return;
    }
    loop {
        let request = match wire_frame::read_line(&mut reader, MAX_FRAME_BYTES) {
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
        let selections = Arc::clone(&selections);
        let _ = std::thread::Builder::new()
            .name("tilo-chooser-req".into())
            .spawn(move || {
                let reply = if matches!(
                    &request,
                    Request::PickContents { .. }
                        | Request::ReadContents { .. }
                        | Request::ReleaseContents { .. }
                ) {
                    contents::answer(&context, &caller, request, &selections)
                } else {
                    answer(&context, &opens, &caller, request)
                };
                send(&writer, &reply);
            });
    }
    selections.close();
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
        Request::PickContents { id, .. }
        | Request::ReadContents { id, .. }
        | Request::ReleaseContents { id, .. } => Reply::Failed {
            id,
            reason: t("portal-chooser-file-grant-unavailable"),
        },
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
        Access::Write
    } else {
        Access::Read
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
                Access::Write
            } else {
                Access::Read
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portal::FilePickerQueue;
    use crate::shell::state::{Capability, FsMode, GRANT_SESSION, Workspace, WorkspaceId};
    use std::io::BufRead as _;
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
        workspace.set_owner(crate::shell::state::local_user_id_for_room(&workspace.room));
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
    #[serial_test::serial(process_env)]
    fn contents_picker_can_review_a_new_file_without_app_filesystem_grants() {
        let session = session_with_save_hint(false);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", &session.elsewhere);
        let stream = UnixStream::connect(&session.socket).unwrap();
        (&stream)
            .write_all(
                &launcher::services::chooser_handshake("so.tilo.notes", &session.room).unwrap(),
            )
            .unwrap();
        let picker = tilo_dialogs::shell::picker(stream, None).unwrap();
        let contents = picker.pick_contents(tilo_dialogs::portal::PickRequest::open(
            "Review package",
            "Review",
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        let request = loop {
            assert!(
                contents.try_recv().is_err(),
                "a new file must reach the owner picker, not fail on empty app grants"
            );
            if let Some(request) = session.pickers.drain().pop() {
                break request;
            }
            assert!(
                Instant::now() < deadline,
                "the ordinary contents chooser was not mounted"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let scope = request
            .scope
            .as_ref()
            .expect("host supplies its own chooser scope");
        assert!(scope.roots.contains(&session.elsewhere));
        request
            .reply
            .send(Some(vec![session.elsewhere.join("secret.xlsx")]))
            .unwrap();
        let chosen = contents.recv_timeout(Duration::from_secs(10)).unwrap();
        let tilo_dialogs::portal::ContentsOutcome::Picked(files) = chosen else {
            panic!("owner-selected contents must be delivered: {chosen:?}");
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].bytes, b"no");
        assert_eq!(
            files[0].location, None,
            "private host paths are not replayable app capabilities"
        );
    }

    fn published_bytes(encoded: &str) -> Vec<u8> {
        use base64::Engine as _;
        use std::io::Read as _;
        let packed = base64::engine::general_purpose::STANDARD
            .decode(encoded.lines().collect::<String>())
            .unwrap();
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(packed.as_slice())
            .take(8 * 1024 * 1024)
            .read_to_end(&mut bytes)
            .unwrap();
        bytes
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn contents_picker_delivers_published_archive_to_normal_store_review() {
        let snapshot = registry_types::CatalogueSnapshot::from_json(&published_bytes(
            include_str!("../../../../../apps/store/fixtures/published/snapshot379.json.gz.b64"),
        ))
        .unwrap();
        let entries = app_cli::client::classify_snapshot(
            &snapshot,
            "55d9a9c748bcfe34200832c18c5f0cf7932f17571612321eaa542474fa2abe06",
            1_791_214_328,
        )
        .unwrap();
        let entry = entries
            .iter()
            .find(|entry| entry.entry.package_id == "so.tilo.store")
            .unwrap();
        let offered = entry
            .entry
            .versions
            .iter()
            .find(|v| {
                v.version.starts_with("0.1.70738+")
                    && v.target().is_some_and(|t| t.architecture == "x86_64")
            })
            .unwrap();
        let offered = offered.tapp().unwrap();
        let bytes = published_bytes(include_str!(
            "../../../../../apps/store/fixtures/published/store70738-x86_64.tapp.gz.b64"
        ));
        assert_eq!(tilo_content::hash_bytes(&bytes), offered.tapp_blake3_hex);
        let mut item = store::sources::tilo_to_store_item(
            entry,
            "Historical independently verified fixture",
            &[],
        );
        item.versions
            .retain(|v| v.root_hash_hex.as_deref() == Some(offered.tapp_root_hash_hex.as_str()));
        let session = session_with_save_hint(false);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", &session.elsewhere);
        let path = session.elsewhere.join("package.tapp");
        std::fs::write(&path, &bytes).unwrap();
        let stream = UnixStream::connect(&session.socket).unwrap();
        (&stream)
            .write_all(
                &launcher::services::chooser_handshake("so.tilo.store", &session.room).unwrap(),
            )
            .unwrap();
        let picker = tilo_dialogs::shell::picker(stream, None).unwrap();
        let chosen = picker.pick_contents(tilo_dialogs::portal::PickRequest::open(
            "Review package",
            "Review",
        ));
        shown(&session).reply.send(Some(vec![path])).unwrap();
        let tilo_dialogs::portal::ContentsOutcome::Picked(mut files) =
            chosen.recv_timeout(Duration::from_secs(10)).unwrap()
        else {
            panic!("actual Store chooser must receive selected bytes")
        };
        assert_eq!(files.len(), 1);
        let selected = files.remove(0);
        assert_eq!(selected.bytes, bytes);
        assert!(selected.location.is_none());
        let review = store::package_import::review(selected.bytes, &[item]).unwrap();
        assert_eq!(
            review.expected.package_root.to_hex(),
            offered.tapp_root_hash_hex
        );
        assert_eq!(review.expected.size_bytes, offered.tapp_size_bytes);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn contents_pending_selection_keeps_the_admitted_pregranted_root() {
        let session = session_with_save_hint(false);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", &session.elsewhere);
        let stream = UnixStream::connect(&session.socket).unwrap();
        (&stream)
            .write_all(&launcher::services::chooser_handshake(APP, &session.room).unwrap())
            .unwrap();
        let picker = tilo_dialogs::shell::picker(stream, None).unwrap();
        let contents =
            picker.pick_contents(tilo_dialogs::portal::PickRequest::open("Review", "Review"));
        let request = shown(&session);
        assert!(
            request
                .scope
                .as_ref()
                .unwrap()
                .roots
                .contains(&session.inbox)
        );
        let retained = session.inbox.with_file_name("retained");
        let outside = session.inbox.with_file_name("private");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("x.xlsx"), b"not the admitted root").unwrap();
        std::fs::rename(&session.inbox, &retained).unwrap();
        std::os::unix::fs::symlink(&outside, &session.inbox).unwrap();
        request
            .reply
            .send(Some(vec![session.inbox.join("x.xlsx")]))
            .unwrap();
        let tilo_dialogs::portal::ContentsOutcome::Picked(files) =
            contents.recv_timeout(Duration::from_secs(10)).unwrap()
        else {
            panic!("admitted directory descriptor must survive a root rename")
        };
        assert_eq!(files.len(), 1);
        assert_eq!(
            files.first().unwrap().bytes,
            b"sheet",
            "replacement root cannot substitute outside contents"
        );
    }

    fn content_request(id: u64) -> Request {
        Request::PickContents {
            id,
            multiple: false,
            title: "Review".into(),
            accept_label: "Review".into(),
            filters: Vec::new(),
        }
    }

    fn selected_grant(
        session: &Session,
        app: &mut App,
        id: u64,
        path: PathBuf,
    ) -> tilo_dialogs::shell::FilePortalGrant {
        app.send(&content_request(id));
        shown(session).reply.send(Some(vec![path])).unwrap();
        match app.reply().unwrap() {
            Reply::ContentsGranted { grants, .. } => {
                assert_eq!(grants.len(), 1);
                grants.into_iter().next().unwrap()
            }
            other => panic!("expected owner-selected grant: {other:?}"),
        }
    }

    fn read_grant(
        app: &mut App,
        grant: &tilo_dialogs::shell::FilePortalGrant,
        offset: u64,
        max_bytes: u32,
    ) -> Reply {
        app.send(&Request::ReadContents {
            id: 99,
            grant: grant.clone(),
            offset,
            max_bytes,
        });
        app.reply().unwrap()
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn contents_grants_bind_connection_identity_offset_and_exact_file() {
        let session = session_with_save_hint(false);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", &session.elsewhere);
        let mut owner = App::launch(&session, "so.tilo.notes", &session.room);
        let grant = selected_grant(
            &session,
            &mut owner,
            1,
            session.elsewhere.join("secret.xlsx"),
        );
        let mut foreign = App::launch(&session, "so.tilo.notes", &session.room);
        assert!(matches!(
            read_grant(&mut foreign, &grant, 0, 1),
            Reply::Failed { .. }
        ));
        assert!(
            matches!(read_grant(&mut owner, &grant, 0, 1), Reply::ContentsChunk { bytes, eof: false, .. } if bytes == b"n")
        );
        assert!(
            matches!(read_grant(&mut owner, &grant, 1, 1), Reply::ContentsChunk { bytes, eof: true, .. } if bytes == b"o")
        );
        assert!(
            matches!(read_grant(&mut owner, &grant, 2, 1), Reply::Failed { .. }),
            "EOF revokes the capability"
        );
        for field in 0..4 {
            let grant = selected_grant(
                &session,
                &mut owner,
                2,
                session.elsewhere.join("secret.xlsx"),
            );
            let mut wrong = grant.clone();
            match field {
                0 => wrong.request = 500,
                1 => wrong.name = "different".into(),
                2 => wrong.size = 600,
                _ => wrong.token = uuid::Uuid::new_v4().to_string(),
            }
            assert!(matches!(
                read_grant(&mut owner, &wrong, 0, 1),
                Reply::Failed { .. }
            ));
            owner.send(&Request::ReleaseContents { id: 100, grant });
            assert!(matches!(
                owner.reply(),
                Some(Reply::ContentsReleased { .. })
            ));
        }
        let grant = selected_grant(
            &session,
            &mut owner,
            3,
            session.elsewhere.join("secret.xlsx"),
        );
        assert!(
            matches!(read_grant(&mut owner, &grant, 1, 1), Reply::Failed { .. }),
            "non-sequential reads revoke"
        );
        let grant = selected_grant(
            &session,
            &mut owner,
            4,
            session.elsewhere.join("secret.xlsx"),
        );
        assert!(matches!(
            read_grant(
                &mut owner,
                &grant,
                0,
                tilo_dialogs::shell::CONTENT_CHUNK_BYTES + 1
            ),
            Reply::Failed { .. }
        ));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn contents_grants_refuse_outside_symlink_oversize_and_changed_files() {
        let session = session_with_save_hint(false);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", &session.elsewhere);
        let mut app = App::launch(&session, "so.tilo.notes", &session.room);
        let link = session.elsewhere.join("link.xlsx");
        std::os::unix::fs::symlink(session.inbox.join("x.xlsx"), &link).unwrap();
        let huge = session.elsewhere.join("huge.tapp");
        std::fs::File::create(&huge)
            .unwrap()
            .set_len(tilo_dialogs::shell::MAX_CONTENT_BYTES + 1)
            .unwrap();
        for path in [session.inbox.join("x.xlsx"), link, huge] {
            app.send(&content_request(1));
            shown(&session).reply.send(Some(vec![path])).unwrap();
            assert!(matches!(app.reply(), Some(Reply::Failed { .. })));
        }
        let path = session.elsewhere.join("secret.xlsx");
        let grant = selected_grant(&session, &mut app, 2, path.clone());
        assert!(matches!(
            read_grant(&mut app, &grant, 0, 1),
            Reply::ContentsChunk { .. }
        ));
        std::fs::write(&path, b"changed").unwrap();
        assert!(matches!(
            read_grant(&mut app, &grant, 1, 1),
            Reply::Failed { .. }
        ));
        assert!(matches!(
            read_grant(&mut app, &grant, 1, 1),
            Reply::Failed { .. }
        ));
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn contents_picker_disconnect_cancels_pending_owner_request() {
        let session = session_with_save_hint(false);
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", &session.elsewhere);
        let mut app = App::launch(&session, "so.tilo.notes", &session.room);
        app.send(&content_request(1));
        let request = shown(&session);
        app.writer.shutdown(std::net::Shutdown::Both).unwrap();
        let deadline = Instant::now().checked_add(Duration::from_secs(2)).unwrap();
        while !request.reply.is_closed() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(
            request.reply.is_closed(),
            "closed caller must revoke the pending chooser"
        );
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
