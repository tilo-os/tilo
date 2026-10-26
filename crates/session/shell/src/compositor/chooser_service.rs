//! The shell's own file choosers, for apps that reach no desktop portal (a
//! provider's headless guest runs no session bus).
//!
//! The launcher connects every app launched into a room and names the app
//! and the room before the sandbox starts (`tilo_dialogs::shell`), so a
//! request is attributed without trusting the app. A chooser is the shell's
//! file picker, on screen, over everything the person can reach; choosing
//! is the person's consent, and gives the app exactly what was chosen,
//! privately on this device, through its documents (`tilo_documents`): the
//! app hears the path inside its sandbox where the choice now is. Where the
//! session serves no documents, or the app's workspace is not the person's
//! on screen, the chooser reaches only what the workspace grants the app,
//! and what comes back is checked against those grants. Opening a file
//! hands it to the compositor thread, which opens it in the app that owns
//! its type; opening a link hands it to the same dispatcher the shell's
//! search opens links with, which starts the app that claims its scheme.
//!
//! Who answers a chooser: a session with someone at it (an attached agent
//! driving the screen, or an ordinary desktop) shows it; an unattended one
//! answers [`Reply::Nobody`] and the app falls back to its unattended picker.
//!
//! An app whose workspace has no identity selected yet waits for one here
//! ([`Request::AwaitIdentity`]), answered the moment the workspace has one,
//! and asks for the shell's identity chooser ([`Request::ChooseIdentity`]):
//! the palette, opened on choosing who to be in the workspace on screen.

mod contents;

use std::io::{BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use collab_types::caps::Access;
use tilo_dialogs::shell::{
    ChosenApp, Handshake, MAX_FRAME_BYTES, PROTOCOL, PickMode, Reply, Request,
};

use crate::portal::{
    FilePickerRequest, FilePickerSpawner, GrantStoreHandle, PickerFilter, PickerScope, PortalCaller,
};
use crate::shell::i18n::{t, t_args};

/// Whether someone is at the computer to answer a chooser.
pub type Presence = Arc<dyn Fn() -> bool + Send + Sync>;

/// Whether the workspace of a room (its id) has an identity selected.
pub type IdentitySelected = Arc<dyn Fn(&str) -> Result<bool, String> + Send + Sync>;

/// What the service needs from the session.
pub struct ChooserContext {
    pub grants: GrantStoreHandle,
    pub pickers: Arc<dyn FilePickerSpawner>,
    pub presence: Presence,
    /// Where an unattended session saves; a Save chooser starts there.
    pub save_directory: Option<PathBuf>,
    /// Wakes the compositor loop to mount a queued chooser or run an open.
    pub wake: crate::session_runtime::Wake,
    /// Read again whenever what decides it may have changed
    /// (`tilo_identity_host::policy_change`).
    pub identity_selected: IdentitySelected,
    /// The files people chose for apps, served into each app's sandbox
    /// (`tilo_documents`). `None` where the session serves no documents
    /// filesystem: a chooser then reaches only what the workspace grants.
    pub documents: Option<tilo_documents::Documents>,
}

/// An app asked for `path` to be opened in the app that owns its type.
/// Answered on the compositor thread, which owns the tree and the launcher.
pub struct OpenRequest {
    pub caller: PortalCaller,
    pub path: PathBuf,
    pub reply: mpsc::Sender<Result<(), String>>,
    /// The path the app named, when it is one it was given through its
    /// documents (`/run/tilo/documents/…`): checked again against that
    /// choice, not the workspace's grants, when it is dispatched.
    given: Option<(tilo_documents::AppKey, PathBuf)>,
}

/// An app asked for `uri` to be opened in the app that claims its scheme.
/// Answered on the compositor thread, like [`OpenRequest`].
pub struct LinkOpenRequest {
    pub caller: PortalCaller,
    pub uri: String,
    pub reply: mpsc::Sender<LinkOpened>,
}

/// An app asked for the identity chooser of its workspace. Answered on the
/// compositor thread, which owns the palette: whether it is on screen.
pub struct IdentityChooserRequest {
    pub caller: PortalCaller,
    pub reply: mpsc::Sender<Result<(), String>>,
}

/// An app asked for the shell's app chooser ([`Request::ChooseApp`]).
/// Answered on the compositor thread, which owns the palette, once the
/// person chose an app (or none).
pub struct AppChooserRequest {
    pub caller: PortalCaller,
    /// What the app will do with the app chosen, in the person's words.
    pub purpose: String,
    pub reply: mpsc::Sender<Result<Option<ChosenApp>, String>>,
}

/// How the shell answered a [`LinkOpenRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOpened {
    Opened,
    /// No app this session knows claims the link; the asking app hands it
    /// to its desktop's opener instead.
    Unclaimed,
    Refused(String),
}

/// Where a connection queues its opens for the compositor thread.
#[derive(Clone)]
struct OpenQueues {
    documents: mpsc::Sender<OpenRequest>,
    links: mpsc::Sender<LinkOpenRequest>,
    identities: mpsc::Sender<IdentityChooserRequest>,
    apps: mpsc::Sender<AppChooserRequest>,
}

pub struct ChooserService {
    grants: GrantStoreHandle,
    documents: Option<tilo_documents::Documents>,
    opens: mpsc::Receiver<OpenRequest>,
    links: mpsc::Receiver<LinkOpenRequest>,
    identities: mpsc::Receiver<IdentityChooserRequest>,
    apps: mpsc::Receiver<AppChooserRequest>,
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
        let (links_tx, links_rx) = mpsc::channel();
        let (identities_tx, identities_rx) = mpsc::channel();
        let (apps_tx, apps_rx) = mpsc::channel();
        let queues = OpenQueues {
            documents: opens_tx,
            links: links_tx,
            identities: identities_tx,
            apps: apps_tx,
        };
        let grants = context.grants.clone();
        let documents = context.documents.clone();
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
                            let opens = queues.clone();
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
            documents,
            opens: opens_rx,
            links: links_rx,
            identities: identities_rx,
            apps: apps_rx,
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
                // workspace policy, or the choice it was given by, rather
                // than handing over stale approval.
                let checked = match &open.given {
                    Some((key, sandbox)) => given_host_path(self.documents.as_ref(), key, sandbox)
                        .map(|path| vec![path]),
                    None => permitted(
                        &self.grants,
                        &open.caller,
                        std::slice::from_ref(&open.path),
                        PickMode::Open,
                    ),
                };
                match checked {
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

impl ChooserService {
    /// The identity choosers apps asked for; each must be answered.
    pub fn drain_identity_choosers(&self) -> Vec<IdentityChooserRequest> {
        self.identities.try_iter().collect()
    }

    /// Open the app choosers apps asked for with `open`, when the asking
    /// app's workspace is `active` (the one on screen); any other is
    /// refused. Whether one opened.
    pub fn serve_app_choosers(
        &self,
        active: crate::shell::state::WorkspaceId,
        mut open: impl FnMut(&str, mpsc::Sender<Result<Option<ChosenApp>, String>>),
    ) -> bool {
        let mut opened = false;
        for request in self.apps.try_iter() {
            if request.caller.workspace_id == active {
                open(&request.purpose, request.reply);
                opened = true;
            } else {
                tracing::info!(app = %request.caller.app_id, "app chooser refused: another workspace");
                let _ = request.reply.send(Err(t("portal-chooser-other-workspace")));
            }
        }
        opened
    }

    /// Open the links apps asked for, through the dispatcher the shell's
    /// search uses, in the asking app's workspace when it is the one on
    /// screen.
    pub fn open_requested_links(&self, chrome: &crate::shell::chrome_state::Chrome) {
        use crate::shell::uri_open::{self, UriOpenError};
        for open in self.links.try_iter() {
            let active = chrome.borrow().tree().desktop.active;
            let outcome = if open.caller.workspace_id == active {
                // Taken to the link when the person's own press in the
                // asking app is behind the request; else it opens beside
                // them.
                let person = crate::compositor::activation::person_initiated(
                    &chrome.borrow().tree(),
                    open.caller.app_id.as_str(),
                );
                match uri_open::open_for_app(&open.uri, chrome, person) {
                    Ok(_) => LinkOpened::Opened,
                    Err(UriOpenError::InvalidUri | UriOpenError::NoHandler { .. }) => {
                        LinkOpened::Unclaimed
                    }
                    Err(UriOpenError::NoWorkspace) => {
                        LinkOpened::Refused(t("portal-chooser-other-workspace"))
                    }
                    Err(UriOpenError::Launch(error)) => LinkOpened::Refused(error.user_message()),
                }
            } else {
                LinkOpened::Refused(t("portal-chooser-other-workspace"))
            };
            if let LinkOpened::Refused(reason) = &outcome {
                tracing::info!(app = %open.caller.app_id, %reason, "app link open refused");
            }
            let _ = open.reply.send(outcome);
        }
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

fn serve_connection(stream: UnixStream, context: &Arc<ChooserContext>, opens: &OpenQueues) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let writer = Arc::new(Mutex::new(writer));
    let mut reader = BufReader::new(stream);
    let Some(handshake) = wire_frame::read_line(&mut reader, MAX_FRAME_BYTES)
        .ok()
        .flatten()
        .and_then(|frame| serde_json::from_slice::<Handshake>(&frame).ok())
    else {
        return;
    };
    let Some(caller) = authenticate(&context.grants, &handshake) else {
        return;
    };
    // The room the launcher named: whose identity this app waits for.
    let room: Arc<str> = Arc::from(handshake.workspace_id);
    let selections = Arc::new(contents::ConnectionGrants::default());
    if selections.maintain().is_err() {
        return;
    }
    let identity_waits = Arc::new(IdentityWaits::default());
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
        let room = Arc::clone(&room);
        let identity_waits = Arc::clone(&identity_waits);
        let _ = std::thread::Builder::new()
            .name("tilo-chooser-req".into())
            .spawn(move || {
                let reply = match request {
                    Request::AwaitIdentity { id } => {
                        await_identity(&context, &room, id, &identity_waits)
                    }
                    request @ (Request::PickContents { .. }
                    | Request::ReadContents { .. }
                    | Request::ReleaseContents { .. }) => {
                        Some(contents::answer(&context, &caller, request, &selections))
                    }
                    request => Some(answer(&context, &opens, &caller, &room, request)),
                };
                if let Some(reply) = reply {
                    send(&writer, &reply);
                }
            });
    }
    identity_waits.close();
    selections.close();
}

/// The identity waits of one connection ([`Request::AwaitIdentity`]): the
/// connection closing ends each of them.
#[derive(Default)]
struct IdentityWaits {
    closed: AtomicBool,
    wakes: Mutex<Vec<mpsc::Sender<()>>>,
}

impl IdentityWaits {
    /// Wake through `wake` when the connection closes; `false` when it
    /// already has.
    fn join(&self, wake: mpsc::Sender<()>) -> bool {
        let Ok(mut wakes) = self.wakes.lock() else {
            return false;
        };
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        wakes.push(wake);
        true
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Ok(mut wakes) = self.wakes.lock() {
            for wake in wakes.drain(..) {
                let _ = wake.send(());
            }
        }
    }
}

/// Answer once `room` has an identity selected: read again on every
/// announcement of a change to what decides it (the identity registry is
/// among them), never on a clock. `None` when the connection closed first:
/// nobody is left to tell.
fn await_identity(
    context: &ChooserContext,
    room: &str,
    id: u64,
    waits: &IdentityWaits,
) -> Option<Reply> {
    let (wake, woken) = mpsc::channel::<()>();
    let watch = {
        let wake = wake.clone();
        tilo_identity_host::policy_change::Watch::new(move || {
            let _ = wake.send(());
        })
    };
    if !waits.join(wake) {
        return None;
    }
    loop {
        // Armed before reading, so a selection made while it reads still
        // wakes the next turn.
        watch.arm();
        if waits.is_closed() {
            return None;
        }
        match (context.identity_selected)(room) {
            Ok(true) => return Some(Reply::IdentitySelected { id }),
            Ok(false) => {}
            Err(reason) => return Some(Reply::Failed { id, reason }),
        }
        if woken.recv().is_err() {
            return None;
        }
    }
}

/// Show the identity chooser for the caller's workspace, when someone is at
/// the computer to choose and that workspace is the one on screen.
fn answer_choose_identity(
    context: &ChooserContext,
    identities: &mpsc::Sender<IdentityChooserRequest>,
    caller: &PortalCaller,
    id: u64,
) -> Reply {
    if !(context.presence)() {
        return Reply::Nobody { id };
    }
    let (reply, outcome) = mpsc::channel();
    if identities
        .send(IdentityChooserRequest {
            caller: caller.clone(),
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

/// Show the app chooser for the caller, when someone is at the computer to
/// choose, and wait for what they choose.
fn answer_choose_app(
    context: &ChooserContext,
    apps: &mpsc::Sender<AppChooserRequest>,
    caller: &PortalCaller,
    id: u64,
    purpose: String,
) -> Reply {
    if !(context.presence)() {
        return Reply::Nobody { id };
    }
    let (reply, outcome) = mpsc::channel();
    if apps
        .send(AppChooserRequest {
            caller: caller.clone(),
            purpose,
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
        Ok(Ok(Some(app))) => Reply::AppChosen { id, app },
        Ok(Ok(None)) => Reply::Cancelled { id },
        Ok(Err(reason)) => Reply::Failed { id, reason },
        Err(_) => Reply::Failed {
            id,
            reason: t("portal-chooser-session-closing"),
        },
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
    opens: &OpenQueues,
    caller: &PortalCaller,
    room: &str,
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
        } => answer_pick(
            context,
            caller,
            room,
            Pick {
                id,
                mode,
                multiple,
                title,
                accept_label,
                suggested_name,
                filters,
            },
        ),
        Request::Open { id, path } => {
            answer_open(context, &opens.documents, caller, room, (id, path))
        }
        Request::OpenUri { id, uri } => answer_open_uri(context, &opens.links, caller, id, uri),
        Request::ChooseIdentity { id } => {
            answer_choose_identity(context, &opens.identities, caller, id)
        }
        Request::ChooseApp { id, purpose } => {
            answer_choose_app(context, &opens.apps, caller, id, purpose)
        }
        // Served by the connection, which knows its room and its waits.
        Request::AwaitIdentity { id } => Reply::Failed {
            id,
            reason: t("portal-chooser-session-closing"),
        },
    }
}

/// A path chooser's request ([`Request::Pick`]).
struct Pick {
    id: u64,
    mode: PickMode,
    multiple: bool,
    title: String,
    accept_label: String,
    suggested_name: Option<String>,
    filters: Vec<tilo_dialogs::shell::Filter>,
}

/// Show the chooser for a path and give the app what the person chose.
///
/// Choosing is the person's consent, like every portal's chooser: the
/// chooser shows everything the person can reach, and what they choose is
/// given to the app that asked, as exactly that and nothing more (the file
/// to read for an Open, the one file to write for a Save, the folder and
/// what is under it for a folder), through its documents
/// ([`tilo_documents`]). The app hears the path it reaches the choice by
/// inside its sandbox. Where the session serves no documents, or the app's
/// workspace is not the person's on screen, the chooser reaches only what
/// the workspace already grants the app.
fn answer_pick(context: &ChooserContext, caller: &PortalCaller, room: &str, pick: Pick) -> Reply {
    let id = pick.id;
    let mode = pick.mode;
    if !(context.presence)() {
        return Reply::Nobody { id };
    }
    if mode == PickMode::WritableDirectory && pick.multiple {
        return Reply::Failed {
            id,
            reason: t("portal-chooser-no-file"),
        };
    }
    if matches!(mode, PickMode::Save | PickMode::WritableDirectory)
        && crate::portal::lockdown::current_policy().disable_save_to_disk
    {
        return Reply::Failed {
            id,
            reason: t("portal-chooser-save-disabled"),
        };
    }
    let giving = context
        .documents
        .as_ref()
        .zip(tilo_documents::AppKey::new(room, caller.app_id.as_str()))
        .filter(|_| contents::owner(context, caller));
    let scope = if giving.is_some() {
        person_scope(context, caller, mode)
    } else if let Some(scope) = authorized_scope(context, caller, mode) {
        scope
    } else {
        return Reply::Failed {
            id,
            reason: t("portal-chooser-nothing-granted"),
        };
    };
    let roots = scope.roots.clone();
    let recent = giving
        .as_ref()
        .map(|(documents, key)| documents.chosen(key))
        .unwrap_or_default();
    let (reply, chosen) = tokio::sync::oneshot::channel();
    context.pickers.spawn(FilePickerRequest {
        caller: caller.clone(),
        parent_window: None,
        title: pick.title,
        save: mode == PickMode::Save,
        directory: matches!(mode, PickMode::Directory | PickMode::WritableDirectory),
        writable_directory: mode == PickMode::WritableDirectory,
        multiple: pick.multiple,
        accept_label: Some(pick.accept_label).filter(|label| !label.trim().is_empty()),
        current_name: pick.suggested_name,
        filters: pick
            .filters
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
        recent,
        reply,
    });
    (context.wake)();
    match chosen.blocking_recv() {
        Ok(Some(paths))
            if paths.len() > 1 && (!pick.multiple || mode == PickMode::WritableDirectory) =>
        {
            Reply::Failed {
                id,
                reason: t("portal-chooser-no-file"),
            }
        }
        Ok(Some(paths)) if !paths.is_empty() => {
            let given = match giving {
                Some((documents, key)) => give(documents, &key, &roots, &paths, mode),
                None => permitted(&context.grants, caller, &paths, mode),
            };
            match given {
                Ok(paths) => Reply::Picked { id, paths },
                Err(reason) => Reply::Failed { id, reason },
            }
        }
        _ => Reply::Cancelled { id },
    }
}

/// The chooser's reach when choosing gives: everything the person can reach
/// (their home and the workspace tree), and what the workspace already
/// grants the app. A Save starts where an unattended session saves.
fn person_scope(context: &ChooserContext, caller: &PortalCaller, mode: PickMode) -> PickerScope {
    let verb = if matches!(mode, PickMode::Save | PickMode::WritableDirectory) {
        Access::Write
    } else {
        Access::Read
    };
    let scope = PickerScope::for_user().with_granted_paths(
        context
            .grants
            .granted_paths(caller)
            .into_iter()
            .filter(|path| context.grants.filesystem_path_allows(caller, path, verb)),
    );
    match (mode, context.save_directory.as_deref()) {
        (PickMode::Save, Some(directory))
            if scope.roots.iter().any(|root| directory.starts_with(root)) =>
        {
            PickerScope {
                initial_dir: directory.to_path_buf(),
                ..scope
            }
        }
        _ => scope,
    }
}

/// Give `key` each chosen path, as what `mode` asked for, and answer the
/// paths it reaches them by. Each must lie within the chooser's `roots`:
/// the chooser is the shell's own, but what it answers is checked like
/// anything else before it becomes a grant.
fn give(
    documents: &tilo_documents::Documents,
    key: &tilo_documents::AppKey,
    roots: &[PathBuf],
    paths: &[PathBuf],
    mode: PickMode,
) -> Result<Vec<PathBuf>, String> {
    let kind = match mode {
        PickMode::Open => tilo_documents::Kind::Read,
        PickMode::Save => tilo_documents::Kind::Save,
        PickMode::Directory => tilo_documents::Kind::Folder { write: false },
        PickMode::WritableDirectory => tilo_documents::Kind::Folder { write: true },
    };
    let roots: Vec<PathBuf> = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .collect();
    paths
        .iter()
        .map(|path| {
            let refused = || {
                t_args(
                    "portal-chooser-file-not-granted",
                    &[("path", path.display().to_string().into())],
                )
            };
            let resolved = match mode {
                PickMode::Save => path.parent().and_then(|folder| folder.canonicalize().ok()),
                PickMode::Open | PickMode::Directory | PickMode::WritableDirectory => {
                    path.canonicalize().ok()
                }
            };
            if !resolved.is_some_and(|resolved| roots.iter().any(|root| resolved.starts_with(root)))
            {
                return Err(refused());
            }
            documents.grant(key, path, kind).map_err(|error| {
                tracing::info!(%error, app = key.app(), "a chosen item was not given");
                refused()
            })
        })
        .collect()
}

fn answer_open_uri(
    context: &ChooserContext,
    links: &mpsc::Sender<LinkOpenRequest>,
    caller: &PortalCaller,
    id: u64,
    uri: String,
) -> Reply {
    // Opening links is a capability like any other the manifest names
    // (`tilo-caps`'s `open-uri`); an app that never declared it opens none.
    if !crate::shell::launcher::package_declares_capability(caller.app_id.as_str(), "open-uri") {
        tracing::info!(app = %caller.app_id, "app link refused: open-uri is not declared");
        return Reply::Failed {
            id,
            reason: t("portal-chooser-open-uri-undeclared"),
        };
    }
    let (reply, outcome) = mpsc::channel();
    if links
        .send(LinkOpenRequest {
            caller: caller.clone(),
            uri,
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
        Ok(LinkOpened::Opened) => Reply::Opened { id },
        Ok(LinkOpened::Unclaimed) => Reply::Unclaimed { id },
        Ok(LinkOpened::Refused(reason)) => Reply::Failed { id, reason },
        Err(_) => Reply::Failed {
            id,
            reason: t("portal-chooser-session-closing"),
        },
    }
}

fn answer_open(
    context: &ChooserContext,
    opens: &mpsc::Sender<OpenRequest>,
    caller: &PortalCaller,
    room: &str,
    (id, path): (u64, PathBuf),
) -> Reply {
    // A path the app was given through its documents is the one it hears
    // and opens itself; the file it names on the host is found through that
    // choice, which the workspace's grants know nothing of.
    let given = path
        .starts_with(tilo_documents::SANDBOX_DIR)
        .then(|| tilo_documents::AppKey::new(room, caller.app_id.as_str()))
        .flatten()
        .map(|key| (key, path.clone()));
    let checked = match &given {
        Some((key, sandbox)) => {
            given_host_path(context.documents.as_ref(), key, sandbox).map(|path| vec![path])
        }
        None => permitted(&context.grants, caller, &[path], PickMode::Open),
    };
    let paths = match checked {
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
            given,
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

/// The host file `sandbox` (a path `key`'s app was given through its
/// documents) really is, found beneath the choice as an open finds it, while
/// that choice still covers it.
fn given_host_path(
    documents: Option<&tilo_documents::Documents>,
    key: &tilo_documents::AppKey,
    sandbox: &Path,
) -> Result<PathBuf, String> {
    documents
        .and_then(|documents| tilo_documents::host::resolve(documents, key, sandbox))
        .and_then(|target| target.real_path().ok())
        .ok_or_else(|| t("portal-chooser-no-file"))
}

/// Derive picker reach from the requested operation's current authority.
fn authorized_scope(
    context: &ChooserContext,
    caller: &PortalCaller,
    mode: PickMode,
) -> Option<PickerScope> {
    let verb = if matches!(mode, PickMode::Save | PickMode::WritableDirectory) {
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
                PickMode::Open | PickMode::Directory | PickMode::WritableDirectory => path
                    .canonicalize()
                    .ok()
                    .filter(|path| mode != PickMode::WritableDirectory || path.is_dir()),
            };
            let verb = if matches!(mode, PickMode::Save | PickMode::WritableDirectory) {
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

    const APP: &str = "so.tilo.sheets";

    struct Session {
        service: ChooserService,
        pickers: FilePickerQueue,
        present: Arc<AtomicBool>,
        wakes: Arc<std::sync::atomic::AtomicU64>,
        selected: Arc<AtomicBool>,
        /// How often the service read whether it has.
        identity_reads: Arc<std::sync::atomic::AtomicU64>,
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
        session_with(hint, None)
    }

    /// [`session_with_save_hint`], giving what is chosen through `documents`.
    fn session_with(hint: bool, documents: Option<tilo_documents::Documents>) -> Session {
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
        workspace.set_owner(crate::shell::state::local_user_id_for_room(
            &workspace.room(),
        ));
        let room = workspace.room().to_string();
        let (grants, _) = GrantStoreHandle::test_with_workspace(workspace);
        let pickers = FilePickerQueue::new();
        let present = Arc::new(AtomicBool::new(true));
        let presence = {
            let present = Arc::clone(&present);
            Arc::new(move || present.load(Ordering::Acquire))
        };
        let wakes = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let wake_counter = Arc::clone(&wakes);
        let selected = Arc::new(AtomicBool::new(false));
        let identity_reads = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let identity_selected = {
            let selected = Arc::clone(&selected);
            let reads = Arc::clone(&identity_reads);
            let room = room.clone();
            Arc::new(move |asked: &str| {
                reads.fetch_add(1, Ordering::AcqRel);
                Ok::<bool, String>(asked == room && selected.load(Ordering::Acquire))
            })
        };
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
                identity_selected,
                documents,
            },
        )
        .unwrap();
        Session {
            service,
            pickers,
            present,
            wakes,
            selected,
            identity_reads,
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
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
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
        tilo_test_env::wait_for("a chooser to be shown", || session.pickers.drain().pop())
    }

    fn open_request(session: &Session) -> OpenRequest {
        tilo_test_env::wait_for("an open to reach the compositor", || {
            session.service.drain_opens().pop()
        })
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

    /// The identity chooser the service asked the compositor for, once it has.
    fn identity_chooser_request(session: &Session) -> IdentityChooserRequest {
        tilo_test_env::wait_for("an identity chooser to reach the compositor", || {
            session.service.drain_identity_choosers().pop()
        })
    }

    #[test]
    fn an_app_waiting_for_its_workspaces_identity_hears_the_moment_one_is_chosen() {
        let session = session_with_save_hint(false);
        let mut app = App::launch(&session, APP, &session.room);
        app.send(&Request::AwaitIdentity { id: 1 });
        let reads = || session.identity_reads.load(Ordering::Acquire);
        tilo_test_env::wait_until("the wait to read the workspace's identity", || reads() >= 1);
        // A change that selects nothing is read again; any answer to the
        // first read went out before this second one.
        tilo_identity_host::policy_change::announce();
        tilo_test_env::wait_until("the wait to read it again", || reads() >= 2);
        app.writer.set_nonblocking(true).unwrap();
        assert!(
            matches!(
                std::io::Read::read(&mut &app.writer, &mut [0_u8; 1]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ),
            "nothing is selected, so nothing is said"
        );
        app.writer.set_nonblocking(false).unwrap();

        session.selected.store(true, Ordering::Release);
        tilo_identity_host::policy_change::announce();
        assert_eq!(app.reply(), Some(Reply::IdentitySelected { id: 1 }));

        app.send(&Request::AwaitIdentity { id: 2 });
        assert_eq!(
            app.reply(),
            Some(Reply::IdentitySelected { id: 2 }),
            "a workspace that has one is answered at once"
        );
    }

    #[test]
    fn an_app_asks_for_the_identity_chooser_of_its_own_workspace() {
        let session = session_with_save_hint(false);
        let mut app = App::launch(&session, APP, &session.room);
        app.send(&Request::ChooseIdentity { id: 3 });
        let request = identity_chooser_request(&session);
        assert_eq!(request.caller.workspace_id, WorkspaceId(7));
        request.reply.send(Ok(())).unwrap();
        assert_eq!(app.reply(), Some(Reply::Opened { id: 3 }));

        session.present.store(false, Ordering::Release);
        app.send(&Request::ChooseIdentity { id: 4 });
        assert_eq!(app.reply(), Some(Reply::Nobody { id: 4 }));
        assert!(session.service.drain_identity_choosers().is_empty());
    }

    #[test]
    fn directory_write_intent_preserves_existing_grant_boundaries_and_cancellation() {
        let session = session_with_save_hint(false);
        let caller = session
            .service
            .grants
            .caller_in_room(&session.room, APP)
            .unwrap();
        let granted_before = session.service.grants.granted_paths(&caller);
        let mut app = App::launch(&session, APP, &session.room);
        app.send(&pick(1, PickMode::Directory));
        let request = shown(&session);
        assert!(request.directory);
        assert!(
            request
                .scope
                .as_ref()
                .unwrap()
                .roots
                .contains(&session.inbox)
        );
        request
            .reply
            .send(Some(vec![session.inbox.clone()]))
            .unwrap();
        assert_eq!(
            app.reply(),
            Some(Reply::Picked {
                id: 1,
                paths: vec![session.inbox.clone()]
            })
        );

        app.send(&pick(2, PickMode::WritableDirectory));
        let request = shown(&session);
        assert!(request.directory);
        assert!(!request.save);
        assert!(request.writable_directory);
        assert_eq!(
            request.display_title(),
            t("portal-file-existing-write-folder")
        );
        assert_eq!(request.confirm_label(), t("portal-file-use-write-folder"));
        assert_eq!(
            request.scope.as_ref().unwrap().roots,
            vec![session.outbox.clone()]
        );
        request
            .reply
            .send(Some(vec![session.inbox.clone()]))
            .unwrap();
        assert!(matches!(app.reply(), Some(Reply::Failed { id: 2, .. })));

        app.send(&pick(3, PickMode::WritableDirectory));
        shown(&session)
            .reply
            .send(Some(vec![session.outbox.clone()]))
            .unwrap();
        assert_eq!(
            app.reply(),
            Some(Reply::Picked {
                id: 3,
                paths: vec![session.outbox.clone()]
            })
        );
        app.send(&pick(4, PickMode::WritableDirectory));
        shown(&session).reply.send(None).unwrap();
        assert_eq!(app.reply(), Some(Reply::Cancelled { id: 4 }));
        assert_eq!(
            session.service.grants.granted_paths(&caller),
            granted_before
        );
        assert!(session.service.grants.filesystem_path_allows(
            &caller,
            &session.outbox,
            Access::Write
        ));
        assert!(!session.service.grants.filesystem_path_allows(
            &caller,
            &session.inbox,
            Access::Write
        ));
        assert!(!session.service.grants.filesystem_path_allows(
            &caller,
            &session.elsewhere,
            Access::Write
        ));
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

        app.send(&pick(2, PickMode::Open));
        shown(&session)
            .reply
            .send(Some(vec![session.elsewhere.join("secret.xlsx")]))
            .unwrap();
        assert!(matches!(app.reply(), Some(Reply::Failed { id: 2, .. })));

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

        app.send(&pick(5, PickMode::Open));
        shown(&session).reply.send(None).unwrap();
        assert_eq!(app.reply(), Some(Reply::Cancelled { id: 5 }));

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
        let request = tilo_test_env::wait_for("the ordinary contents chooser to mount", || {
            assert!(
                contents.try_recv().is_err(),
                "a new file must reach the owner picker, not fail on empty app grants"
            );
            session.pickers.drain().pop()
        });
        let scope = request
            .scope
            .as_ref()
            .expect("host supplies its own chooser scope");
        assert!(scope.roots.contains(&session.elsewhere));
        request
            .reply
            .send(Some(vec![session.elsewhere.join("secret.xlsx")]))
            .unwrap();
        let chosen = tilo_test_env::recv("the chosen contents", &contents);
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

    /// A complete archive and the Store offer for it, made the way the
    /// registry makes one: an owner-signed catalogue lists a publisher-signed
    /// entry naming the archive's size and hash, and the offer comes from
    /// verifying that catalogue.
    fn offered_archive() -> (Vec<u8>, store::StoreItem, registry_types::TappArtifact) {
        use ed25519_dalek::SigningKey;
        #[cfg(target_arch = "aarch64")]
        let (arch, isa, target_isa) = ("aarch64", "armv8-a", "aarch64");
        #[cfg(not(target_arch = "aarch64"))]
        let (arch, isa, target_isa) = ("x86_64", "baseline", "x86_64");
        let publisher = SigningKey::from_bytes(&[76; 32]);
        let identity = tilo_app_package::SigningIdentity::from_signing_key(publisher.clone());
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        std::fs::write(
            dir.path().join("bin/app"),
            b"#!/bin/true\nchooser-fixture\n",
        )
        .unwrap();
        let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            r#"[package]
id = "so.tilo.chooser-fixture"
version = "1.0.0"
kind = "app"
name = "Chooser fixture"
[package.isa]
{arch} = "{isa}"
[publisher]
identity = "ed25519:{}"
signature = ""
[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[entrypoint]
bin = "bin/app"
type = "gui"
"#,
            hex::encode(identity.public_key_bytes())
        ))
        .unwrap();
        let packed = tilo_app_package::pack(dir.path(), &manifest, &identity).unwrap();
        let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
        let offered = registry_types::TappArtifact {
            tapp_url: "https://packages.test/chooser-fixture.tapp".into(),
            tapp_root_hash_hex: packed.id().to_hex(),
            tapp_size_bytes: bytes.len() as u64,
            tapp_blake3_hex: tilo_content::hash_bytes(&bytes),
            target: registry_types::RegistryTarget {
                platform: registry_types::TILO_OS_PLATFORM.into(),
                architecture: arch.into(),
                isa: target_isa.into(),
                runtime: None,
                interfaces: Vec::new(),
            },
        };
        let mut entry = registry_types::RegistryEntry {
            package_id: manifest.package.id,
            kind: registry_types::EntryKind::App,
            name: "Chooser fixture".into(),
            description: "A signed test app".into(),
            tagline: "Signed and listed".into(),
            images: registry_types::LocalizedImages::default(),
            supported_languages: vec!["en".into()],
            categories: Vec::new(),
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
            versions: vec![registry_types::RegistryVersion {
                version: "1.0.0".into(),
                released_at_unix: 1,
                artifact: registry_types::VersionArtifact::Tapp(offered.clone()),
            }],
            updated_at_unix: 2,
            signature_hex: String::new(),
        };
        registry_types::sign_entry(&mut entry, &publisher);
        let owner = SigningKey::from_bytes(&[77; 32]);
        let now = 1_800_000_000;
        let mut snapshot = registry_types::SignedRegistrySnapshot {
            protocol: registry_types::PACKAGE_FORMAT_VERSION,
            sequence: 1,
            issued_at_unix: now,
            expires_at_unix: now + 3600,
            previous_snapshot_hash_hex: String::new(),
            moderation_head_hex: String::new(),
            publishers: vec![registry_types::PublisherAuthorization {
                pubkey_hex: entry.publisher_pubkey_hex.clone(),
                can_add_new: true,
            }],
            entries: vec![entry],
            owner_signature_hex: String::new(),
            channel_authorization: None,
        };
        registry_types::sign_snapshot(&mut snapshot, &owner).unwrap();
        let entries = app_cli::client::classify_snapshot(
            &registry_types::CatalogueSnapshot::Full(snapshot),
            &hex::encode(owner.verifying_key().to_bytes()),
            now,
        )
        .unwrap();
        let item = store::sources::tilo_to_store_item(&entries[0], "Signed catalogue", &[]);
        (bytes, item, offered)
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn contents_picker_delivers_offered_archive_to_normal_store_review() {
        let (bytes, item, offered) = offered_archive();
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
            tilo_test_env::recv("the chosen contents", &chosen)
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
            tilo_test_env::recv("the chosen contents", &contents)
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
        tilo_test_env::wait_until("the closed caller to revoke the pending chooser", || {
            request.reply.is_closed()
        });
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
        tilo_test_env::wait_until("the open to be queued", || {
            session.wakes.load(Ordering::Acquire) != 0
        });
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

    /// Remembers what reached the app's own (desktop) opener.
    #[derive(Default)]
    struct DesktopOpener(Mutex<Vec<String>>);

    impl tilo_dialogs::UriOpener for DesktopOpener {
        fn open(&self, uri: &str) -> std::io::Result<()> {
            self.0.lock().unwrap().push(uri.to_owned());
            Ok(())
        }
    }

    /// Video Player's "Play more video and audio formats" and Camera's
    /// privacy link open Settings through the dispatcher the shell's search
    /// uses, never the desktop's opener, which has no `settings:` handler; a
    /// link nothing in the session claims still goes to the desktop, and an
    /// app whose manifest does not declare `open-uri` opens nothing.
    #[test]
    #[serial_test::serial(process_env, launch_events)]
    fn an_app_link_opens_in_the_app_that_claims_its_scheme() {
        use crate::shell::chrome_state::ChromeState;
        use crate::shell::launcher as shell_launcher;
        use crate::shell::state::{WorkspaceTree, WorkspaceTreeHandle};
        use std::fmt::Write as _;
        use std::os::unix::fs::PermissionsExt as _;

        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let apps = base.join("apps");
        // Settings claims `settings:`; Video Player and Camera declare
        // `open-uri`; Calculator opens no links and declares nothing.
        let mut registry = String::new();
        for (package, app) in [
            ("so.tilo.settings", "settings"),
            ("so.tilo.video-player", "video-player"),
            ("so.tilo.camera", "camera"),
            ("so.tilo.calculator", "calculator"),
        ] {
            let payload = apps.join("payloads").join(package);
            std::fs::create_dir_all(payload.join("bin")).unwrap();
            let manifest = payload.join("manifest.toml");
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../apps")
                    .join(app)
                    .join("tilo-app.toml"),
                &manifest,
            )
            .unwrap();
            write!(
                registry,
                "[[entry]]\nid = '{package}'\npayload_root = {:?}\nmanifest_path = {:?}\n",
                payload.display().to_string(),
                manifest.display().to_string(),
            )
            .unwrap();
        }
        std::fs::write(apps.join("registry.toml"), registry).unwrap();
        let (data, config) = (base.join("data"), base.join("config"));
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&config).unwrap();
        let log = base.join("launched.log");
        let bin = base.join("tilo-launcher");
        std::fs::write(
            &bin,
            format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_APPS_DATA_DIR", &apps)
            .set("TILO_LAUNCHER_BIN", &bin)
            .set("XDG_RUNTIME_DIR", &base)
            .set("XDG_DATA_HOME", &data)
            .set("XDG_DATA_DIRS", &data)
            .set("XDG_CONFIG_HOME", &config)
            .set("XDG_CONFIG_DIRS", &config);
        crate::shell::host_apps::invalidate_cache();
        shell_launcher::set_test_execution_program(Some(bin));
        shell_launcher::set_wayland_socket_path(Some(base.join("wayland-0")));
        shell_launcher::invalidate_app_cache();
        shell_launcher::clear_pending_placements();
        let _ = shell_launcher::drain_events();

        let tree = WorkspaceTreeHandle::new(WorkspaceTree::bootstrap_demo());
        crate::shell::action_authority::follow_tree(&tree);
        let chrome = ChromeState::new_with_handle(tree.clone());
        let room = chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .unwrap()
            .room()
            .to_string();
        let socket = base.join("chooser.sock");
        let service = ChooserService::start_at(
            socket.clone(),
            ChooserContext {
                grants: GrantStoreHandle::new(tree),
                pickers: Arc::new(FilePickerQueue::new()),
                presence: Arc::new(|| true),
                save_directory: None,
                wake: Arc::new(|| {}),
                identity_selected: Arc::new(|_: &str| Ok::<bool, String>(false)),
                documents: None,
            },
        )
        .unwrap();
        let desktop = Arc::new(DesktopOpener::default());
        // Each app's end: the connection its launcher opened and named, and
        // the opener its toolkit builds on it.
        let opener_of = |app_id: &str| {
            let mut stream = UnixStream::connect(&socket).unwrap();
            stream
                .write_all(&launcher::services::chooser_handshake(app_id, &room).unwrap())
                .unwrap();
            tilo_dialogs::shell::ShellLink::connect(stream)
                .unwrap()
                .opener(Arc::clone(&desktop) as Arc<dyn tilo_dialogs::UriOpener>)
        };
        let open = |opener: &Arc<dyn tilo_dialogs::UriOpener>, uri: &'static str| {
            let opener = Arc::clone(opener);
            let asked = std::thread::spawn(move || opener.open(uri).map_err(|e| e.to_string()));
            tilo_test_env::wait_until(format!("{uri} to be answered"), || {
                service.open_requested_links(&chrome);
                asked.is_finished()
            });
            asked.join().unwrap()
        };
        let player = opener_of("so.tilo.video-player");
        let camera = opener_of("so.tilo.camera");

        assert_eq!(open(&player, "settings://updates"), Ok(()));
        assert_eq!(open(&camera, "settings://privacy/camera"), Ok(()));
        let commands = tilo_test_env::wait_for("Settings to be launched twice", || {
            let commands: Vec<String> = std::fs::read_to_string(&log)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            (commands.len() >= 2).then_some(commands)
        });
        for uri in ["settings://updates", "settings://privacy/camera"] {
            assert!(
                commands
                    .iter()
                    .any(|command| command.contains("--app-id so.tilo.settings")
                        && command.contains(&format!("--open-uri {uri}"))),
                "Settings is launched on {uri}: {commands:#?}"
            );
        }
        assert!(
            desktop.0.lock().unwrap().is_empty(),
            "a link a Tilo app claims never reaches the desktop's opener"
        );

        assert_eq!(open(&player, "mailto:ada@example.test"), Ok(()));
        assert_eq!(*desktop.0.lock().unwrap(), ["mailto:ada@example.test"]);

        let calculator = opener_of("so.tilo.calculator");
        assert!(
            open(&calculator, "settings://updates").is_err(),
            "an app that never declared open-uri opens no links"
        );
        assert_eq!(
            desktop.0.lock().unwrap().len(),
            1,
            "nor does the refusal send its link to the desktop's opener"
        );
        assert_eq!(
            std::fs::read_to_string(&log).unwrap().lines().count(),
            2,
            "Settings was not launched for it"
        );

        shell_launcher::set_test_execution_program(None);
        shell_launcher::set_wayland_socket_path(None);
        shell_launcher::invalidate_app_cache();
        shell_launcher::clear_pending_placements();
        let _ = shell_launcher::drain_events();
    }

    #[test]
    fn a_launch_into_a_room_this_session_lacks_is_hung_up_on() {
        let session = session_with_save_hint(true);
        let mut app = App::launch(&session, APP, "not-a-room");
        app.send(&pick(1, PickMode::Open));
        assert_eq!(app.reply(), None);
        assert!(session.pickers.is_empty());
    }

    /// The PDF Viewer, Settings, Backups and Logs: apps the room granted
    /// nothing.
    const VIEWER: &str = "so.tilo.pdf-viewer";
    const SETTINGS: &str = "so.tilo.settings";
    const BACKUPS: &str = "so.tilo.backups";
    const LOGS: &str = "so.tilo.logs";

    /// A session that gives what is chosen, as the shell does: its
    /// documents, mounted where the launcher binds each app's view from
    /// when this machine can mount FUSE, and the person's home with a
    /// report, an image, a backups folder and files beside them.
    struct Giving {
        session: Session,
        documents: tilo_documents::Documents,
        mounted: Option<tilo_documents::mount::Mounted>,
        _env: tilo_test_env::ScopedEnv,
    }

    fn giving() -> Giving {
        let documents = tilo_documents::Documents::new();
        let session = session_with(false, Some(documents.clone()));
        let home = &session.elsewhere;
        std::fs::write(home.join("report.pdf"), b"%PDF").unwrap();
        std::fs::write(home.join("beside.txt"), b"beside").unwrap();
        std::fs::write(home.join("dunes.png"), b"\x89PNG").unwrap();
        std::fs::create_dir_all(home.join("Backups/2026")).unwrap();
        std::fs::write(home.join("Backups/2026/monday.tar"), b"monday").unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("HOME", home);
        let at = session.socket.with_file_name("documents");
        let mounted = match tilo_documents::mount::mount(documents.clone(), &at) {
            Ok(mounted) => Some(mounted),
            Err(error) => {
                tilo_test_env::capability_unavailable(
                    tilo_test_env::Capability::Fuse,
                    format_args!(
                        "FUSE is unavailable here ({error}); checking the documents table only"
                    ),
                );
                None
            }
        };
        Giving {
            session,
            documents,
            mounted,
            _env: env,
        }
    }

    impl Giving {
        fn key(&self, app: &str) -> tilo_documents::AppKey {
            tilo_documents::AppKey::new(&self.session.room, app).unwrap()
        }

        /// Where `app` finds `given` with its view bound as the launcher
        /// binds it, when the documents are mounted.
        fn in_view(&self, app: &str, given: &Path) -> Option<PathBuf> {
            let mounted = self.mounted.as_ref()?;
            let rest = given.strip_prefix(tilo_documents::SANDBOX_DIR).unwrap();
            Some(tilo_documents::app_view(mounted.path(), &self.key(app)).join(rest))
        }

        /// What `app` reads at `given`: through the documents table, and
        /// through its view when mounted, which must agree.
        fn read(&self, app: &str, given: &Path) -> Option<Vec<u8>> {
            let table = tilo_documents::host::resolve(&self.documents, &self.key(app), given)
                .and_then(|target| std::fs::read(target.host_path()).ok());
            if let Some(path) = self.in_view(app, given) {
                assert_eq!(
                    std::fs::read(path).ok(),
                    table,
                    "the mount and the table agree on {}",
                    given.display()
                );
            }
            table
        }

        /// The app's end of the chooser, as its toolkit builds it.
        fn client(&self, app: &str) -> Arc<dyn tilo_dialogs::FilePicker> {
            let mut stream = UnixStream::connect(&self.session.socket).unwrap();
            stream
                .write_all(&launcher::services::chooser_handshake(app, &self.session.room).unwrap())
                .unwrap();
            tilo_dialogs::shell::ShellLink::connect(stream)
                .unwrap()
                .picker(None)
        }

        /// Ask through `app`'s toolkit, choose `chosen` on the shell's
        /// chooser, and answer the path the app hears.
        fn choose(
            &self,
            app: &str,
            request: tilo_dialogs::portal::PickRequest,
            chosen: &Path,
        ) -> PathBuf {
            let picker = self.client(app);
            let asked = picker.pick(request);
            shown(&self.session)
                .reply
                .send(Some(vec![chosen.to_path_buf()]))
                .unwrap();
            match tilo_test_env::recv("the chooser's answer", &asked) {
                tilo_dialogs::portal::PortalOutcome::Opened(path) => path,
                other => panic!("{app} hears what was chosen: {other:?}"),
            }
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn choosing_a_file_in_the_picker_gives_the_app_that_file() {
        let giving = giving();
        let session = &giving.session;
        let report = session.elsewhere.join("report.pdf");
        let mut app = App::launch(session, VIEWER, &session.room);
        assert!(giving.documents.tokens(&giving.key(VIEWER)).is_empty());

        app.send(&pick(1, PickMode::Open));
        let request = shown(session);
        let roots = request.scope.as_ref().map(|scope| scope.roots.clone());
        assert!(
            roots
                .as_ref()
                .is_some_and(|roots| roots.iter().any(|root| session.elsewhere.starts_with(root))),
            "the chooser shows the person's files, not only what the app was granted: {roots:?}"
        );
        request.reply.send(Some(vec![report.clone()])).unwrap();
        let Some(Reply::Picked { id: 1, paths }) = app.reply() else {
            panic!("the app hears what was chosen");
        };
        let [given] = paths.as_slice() else {
            panic!("one file was chosen: {paths:?}");
        };
        assert!(given.starts_with(tilo_documents::SANDBOX_DIR), "{given:?}");
        assert_eq!(given.file_name(), report.file_name());
        assert_eq!(giving.read(VIEWER, given).as_deref(), Some(&b"%PDF"[..]));

        // Exactly that file, to read, and nothing else became readable.
        let target =
            tilo_documents::host::resolve(&giving.documents, &giving.key(VIEWER), given).unwrap();
        assert!(!target.writable(), "an Open gives the file to read");
        if let Some(path) = giving.in_view(VIEWER, given) {
            assert!(std::fs::write(&path, b"changed").is_err());
            assert_eq!(std::fs::read(&report).unwrap(), b"%PDF");
        }
        for beside in ["beside.txt", "secret.xlsx", "Backups"] {
            assert_eq!(
                giving.read(VIEWER, &given.with_file_name(beside)),
                None,
                "{beside} beside the chosen file"
            );
        }
        assert_eq!(
            giving.read(SETTINGS, given),
            None,
            "another app in the room was given nothing"
        );
        assert_eq!(giving.documents.tokens(&giving.key(VIEWER)).len(), 1);

        // Choosing it again reopens it where it was: a recent file works.
        app.send(&pick(2, PickMode::Open));
        shown(session).reply.send(Some(vec![report])).unwrap();
        assert_eq!(
            app.reply(),
            Some(Reply::Picked {
                id: 2,
                paths: vec![given.clone()],
            })
        );
        assert_eq!(giving.documents.tokens(&giving.key(VIEWER)).len(), 1);
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn the_pdf_viewer_opens_a_pdf_it_was_granted_nothing_for() {
        let giving = giving();
        let given = giving.choose(
            VIEWER,
            tilo_dialogs::portal::PickRequest::open("Open a PDF", "Open"),
            &giving.session.elsewhere.join("report.pdf"),
        );
        assert_eq!(giving.read(VIEWER, &given).as_deref(), Some(&b"%PDF"[..]));
    }

    /// An app asking for a file it was given to be opened in the app for
    /// its type names it by the path it heard; the compositor gets the file
    /// that path names on the host, and only for the app it was given to.
    #[test]
    #[serial_test::serial(process_env)]
    fn opening_a_given_file_hands_the_compositor_the_file_it_names() {
        let giving = giving();
        let session = &giving.session;
        let report = session.elsewhere.join("report.pdf");
        let given = giving.choose(
            VIEWER,
            tilo_dialogs::portal::PickRequest::open("Open a PDF", "Open"),
            &report,
        );

        let mut viewer = App::launch(session, VIEWER, &session.room);
        viewer.send(&Request::Open {
            id: 1,
            path: given.clone(),
        });
        let open = open_request(session);
        assert_eq!(open.caller.app_id.as_str(), VIEWER);
        assert_eq!(open.path, report.canonicalize().unwrap());
        open.reply.send(Ok(())).unwrap();
        assert_eq!(viewer.reply(), Some(Reply::Opened { id: 1 }));

        let mut settings = App::launch(session, SETTINGS, &session.room);
        settings.send(&Request::Open {
            id: 2,
            path: given.clone(),
        });
        assert!(
            matches!(settings.reply(), Some(Reply::Failed { id: 2, .. })),
            "another app cannot open what was given to the viewer"
        );
        viewer.send(&Request::Open {
            id: 3,
            path: given.with_file_name("beside.txt"),
        });
        assert!(
            matches!(viewer.reply(), Some(Reply::Failed { id: 3, .. })),
            "nothing beside the given file opens"
        );
        assert!(session.service.drain_opens().is_empty());
    }

    /// Settings' wallpaper chooser opens the chosen image itself
    /// (`send_open_file`): the same request on the wire, then
    /// `open_regular_file` on the path it hears, which inside the sandbox is
    /// in its view.
    #[test]
    #[serial_test::serial(process_env)]
    fn settings_sets_a_wallpaper_from_an_image_it_was_granted_nothing_for() {
        let giving = giving();
        let given = giving.choose(
            SETTINGS,
            tilo_dialogs::portal::PickRequest::open("Choose a wallpaper", "Choose"),
            &giving.session.elsewhere.join("dunes.png"),
        );
        assert_eq!(
            giving.read(SETTINGS, &given).as_deref(),
            Some(&b"\x89PNG"[..])
        );
        if let Some(path) = giving.in_view(SETTINGS, &given) {
            let mut bytes = Vec::new();
            std::io::Read::read_to_end(
                &mut tilo_dialogs::portal::open_regular_file(&path).unwrap(),
                &mut bytes,
            )
            .unwrap();
            assert_eq!(bytes, b"\x89PNG");
        }
    }

    /// Backups chooses a folder: the folder and everything under it, to
    /// read, and nothing above or beside it.
    #[test]
    #[serial_test::serial(process_env)]
    fn backups_chooses_a_folder_and_is_given_its_subtree() {
        let giving = giving();
        let folder = giving.session.elsewhere.join("Backups");
        let given = giving.choose(
            BACKUPS,
            tilo_dialogs::portal::PickRequest::directory("Choose a backup folder", "Choose"),
            &folder,
        );
        assert_eq!(given.file_name(), folder.file_name());
        assert_eq!(
            giving
                .read(BACKUPS, &given.join("2026/monday.tar"))
                .as_deref(),
            Some(&b"monday"[..])
        );
        assert_eq!(giving.read(BACKUPS, &given.join("../report.pdf")), None);
        assert_eq!(
            giving.read(BACKUPS, &given.with_file_name("report.pdf")),
            None
        );
        if let Some(path) = giving.in_view(BACKUPS, &given) {
            let names: Vec<_> = std::fs::read_dir(&path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            assert_eq!(names, vec![std::ffi::OsString::from("2026")]);
            assert!(
                std::fs::write(path.join("2026/tuesday.tar"), b"t").is_err(),
                "a folder chosen to read is not written"
            );
        }
    }

    /// Logs exports to a file that does not exist yet: that one file, to
    /// write, and the save sibling a whole-file replace goes through.
    #[test]
    #[serial_test::serial(process_env)]
    fn logs_exports_to_a_new_file_it_was_granted_nothing_for() {
        let giving = giving();
        let destination = giving.session.elsewhere.join("journal-filtered.jsonl");
        let given = giving.choose(
            LOGS,
            tilo_dialogs::portal::PickRequest::save(
                "Export filtered logs",
                "Export",
                "journal-filtered.jsonl",
            ),
            &destination,
        );
        assert_eq!(given.file_name(), destination.file_name());
        let target =
            tilo_documents::host::resolve(&giving.documents, &giving.key(LOGS), &given).unwrap();
        assert!(target.writable(), "a Save gives the file to write");
        assert_eq!(target.host_path(), destination);
        assert_eq!(
            giving.read(LOGS, &given.with_file_name("beside.txt")),
            None,
            "nothing beside it"
        );
        if let Some(path) = giving.in_view(LOGS, &given) {
            std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)
                .and_then(|mut file| file.write_all(b"{\"line\":1}\n"))
                .unwrap();
            assert_eq!(std::fs::read(&destination).unwrap(), b"{\"line\":1}\n");
            tilo_dialogs::portal::replace_saved(&path, b"{\"line\":2}\n").unwrap();
            assert_eq!(std::fs::read(&destination).unwrap(), b"{\"line\":2}\n");
            assert!(std::fs::write(path.with_file_name("other.jsonl"), b"x").is_err());
            assert!(!giving.session.elsewhere.join("other.jsonl").exists());
        }
    }
}
