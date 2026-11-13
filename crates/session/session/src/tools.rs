//! The tools service: the typed opens an app asks this session for.
//!
//! An app the launcher starts is handed the `tools` socket ([`runtime_portal::tools`]) and asks
//! for what only the session can do: open a tool at a typed context, list or open the drives the
//! person plugged in, list or uninstall the person's apps ([`ToolIntent`] is the whole
//! vocabulary). Who asks is read from the connection's peer credentials, and the request must
//! belong to the workspace that launch recorded.

use std::io::{BufRead, BufReader, ErrorKind, Read as _, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

use runtime_portal::tools::{
    AppEntry, DriveEntry, MAX_LISTED_APPS, MAX_LISTED_DRIVES, MAX_REPLY_BYTES, MAX_WIRE_BYTES,
    TOOL_PROTOCOL_VERSION, ToolIntent, ToolReply, ToolRequest, WorkspaceSettingsRoute,
};
use tilo_action_broker::services::{
    ApplicationChange, ApplicationServices, DriveChange, DriveServices,
};
use tilo_app_launch::Launch;
use tilo_broker_core::services::LaunchPlace;
use tilo_desktop_model::app::AppKind as Builtin;
use tilo_lock::MutexRecover as _;
use tilo_service::Wake;

use crate::Launches;
use crate::drives::{Medium, files_on};

/// The app the person's own app list is answered to. Every other app is told nothing of it.
fn files() -> String {
    String::from(Builtin::FileExplorer.app_id())
}

/// The `tools` socket's name: the launcher hands an app its number as `TILO_SVC_TOOLS_FD` .
pub(crate) const SERVICE: &str = "tools";

/// How long a request that only a sheet on the loop can answer waits for that pass.
const ANSWER_WAIT: Duration = Duration::from_secs(2);

/// A question the tools service leaves for the session's loop: only a pass of the loop can put a
/// sheet up, or read the defaults of the workspace an open belongs to. Each ask carries the
/// channel its answer goes back on.
pub(crate) enum Ask {
    /// Show workspace settings for `workspace` , on `route` 's tab.
    WorkspaceSettings {
        workspace: String,
        route: WorkspaceSettingsRoute,
        reply: SyncSender<Result<(), String>>,
    },
    /// Ask the person whether to uninstall `app` , in the sheet that owns that question.
    Uninstall {
        app: String,
        reply: SyncSender<Result<(), String>>,
    },
    /// The apps that open `path` in `workspace` , the workspace's own default first.
    Openers {
        path: std::path::PathBuf,
        workspace: String,
        reply: SyncSender<Vec<Launch>>,
    },
}

/// What the session's loop and the broker reach of the service.
#[derive(Clone)]
pub(crate) struct Handle {
    inner: Arc<Inner>,
}

struct Inner {
    launches: Arc<Launches>,
    /// What wakes the loop when a sheet is left for it.
    wake: Wake,
    /// The sheets the service left for the loop, until its next pass takes them.
    asks: std::sync::Mutex<Vec<Ask>>,
    /// The drives and the apps as they were last told.
    now: std::sync::Mutex<Now>,
    /// The revision of each list, which Files' waits follow.
    drives_changed: tokio::sync::watch::Sender<u64>,
    apps_changed: tokio::sync::watch::Sender<u64>,
}

#[derive(Default)]
struct Now {
    media: Vec<Medium>,
    apps: Option<Vec<AppEntry>>,
}

/// The tools service, served until the session ends.
pub(crate) struct Tools {
    handle: Handle,
}

impl Handle {
    /// A handle whose lists the loop keeps: nothing is served until [`Tools::serve`] binds the
    /// socket.
    pub(crate) fn new(launches: Arc<Launches>, wake: Wake) -> Self {
        Self {
            inner: Arc::new(Inner {
                launches,
                wake,
                asks: std::sync::Mutex::new(Vec::new()),
                now: std::sync::Mutex::new(Now::default()),
                // One before the first wait (which asks from 0), so a first ask is answered at once
                // rather than waited on.
                drives_changed: tokio::sync::watch::channel(1).0,
                apps_changed: tokio::sync::watch::channel(1).0,
            }),
        }
    }

    /// The media attached now, as the session's own list of them reads.
    pub(crate) fn saw_media(&self, media: Vec<Medium>) {
        {
            let mut now = self.inner.now.lock_recover();
            if now.media == media {
                return;
            }
            now.media = media;
        }
        bump(&self.inner.drives_changed);
    }

    /// The person's apps, as the session's lists last read them.
    pub(crate) fn saw_apps(&self, apps: &[AppEntry]) {
        {
            let mut now = self.inner.now.lock_recover();
            if now.apps.as_deref() == Some(apps) {
                return;
            }
            now.apps = Some(apps.to_vec());
        }
        bump(&self.inner.apps_changed);
    }

    /// The sheet questions left for the loop since its last pass: taken once,
    /// by the pass that puts them up.
    pub(crate) fn take_asks(&self) -> Vec<Ask> {
        std::mem::take(&mut *self.inner.asks.lock_recover())
    }
}

impl Tools {
    /// Bind the tools socket and serve it on threads of its own.
    ///
    /// # Errors
    /// The services root or the socket cannot be made, or the acceptor
    /// cannot start.
    #[allow(
        clippy::disallowed_methods,
        reason = "session helper: the tools service accepts and serves on its own threads"
    )]
    pub(crate) fn serve(handle: Handle) -> Result<Self, String> {
        launcher::services::ensure_services_root()
            .map_err(|error| format!("create the services directory: {error}"))?;
        let path = launcher::services::socket_for(SERVICE);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove the stale tools socket: {error}"))?;
        }
        let listener = tilo_files::native::socket::bind(&path)
            .map_err(|error| format!("bind the tools socket {}: {error}", path.display()))?;
        let accepting = handle.clone();
        std::thread::Builder::new()
            .name("tilo-tools".into())
            .spawn(move || accept(&listener, &accepting))
            .map_err(|error| format!("start the tools service: {error}"))?;
        tracing::info!(path = %path.display(), "serving an app's typed opens");
        Ok(Self { handle })
    }

    /// What the loop and the broker reach of the service.
    pub(crate) fn handle(&self) -> &Handle {
        &self.handle
    }
}

impl DriveServices for Handle {
    fn changed(&self, after: u64) -> Option<DriveChange> {
        Some(wait(&self.inner.drives_changed, after))
    }
}

impl ApplicationServices for Handle {
    fn changed(&self, after: u64) -> Option<ApplicationChange> {
        Some(wait(&self.inner.apps_changed, after))
    }
}

/// Move `revision` on by one, waking the waits that follow it.
fn bump(revision: &tokio::sync::watch::Sender<u64>) {
    revision.send_modify(|revision| *revision = revision.wrapping_add(1).max(1));
}

/// Resolves once `revision` is past `after` , with the revision to wait from next. Subscribed now,
/// so a change in between is still seen.
fn wait(revision: &tokio::sync::watch::Sender<u64>, after: u64) -> DriveChange {
    let mut changes = revision.subscribe();
    Box::pin(async move {
        loop {
            let current = *changes.borrow_and_update();
            if current > after {
                return current;
            }
            if changes.changed().await.is_err() {
                // The revision outlives every wait and never closes.
                std::future::pending::<()>().await;
            }
        }
    })
}

/// Accept an app's connection until the session ends.
#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the tools service accepts and serves on its own threads"
)]
fn accept(listener: &UnixListener, handle: &Handle) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let handle = handle.clone();
                if let Err(error) = std::thread::Builder::new()
                    .name("tilo-tools-app".into())
                    .spawn(move || serve(&handle, stream))
                {
                    tracing::warn!(%error, "a tool connection could not be served");
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
                tracing::warn!(%error, "the tools service's accept failed");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// Who asked: the app this session launched, and the workspace its launch recorded. Read from the
/// connection, never from the request.
struct Caller {
    app: String,
    workspace: String,
}

/// Serve one app's connection: its requests, answered one line at a time.
fn serve(handle: &Handle, stream: UnixStream) {
    let inner = Arc::clone(&handle.inner);
    let caller = caller_of(&inner, &stream);
    let Ok(mut writing) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    loop {
        match read_request(&mut reader) {
            Line::Done => return,
            Line::TooLarge => {
                let _ = tilo_failure::ignored!(
                    "the connection ends whether or not it read the refusal",
                    say(&mut writing, &refused("the tool request is too large"))
                );
                return;
            }
            Line::Refused(message) => {
                if !say(&mut writing, &refused(message)) {
                    return;
                }
            }
            Line::Request(request) => {
                let reply = answer(&inner, caller.as_ref(), &request);
                if !say(&mut writing, &reply) {
                    return;
                }
            }
        }
    }
}

/// Write one reply as its line, `false` when the app stopped reading.
fn say(writing: &mut UnixStream, reply: &ToolReply) -> bool {
    let Ok(mut line) = serde_json::to_vec(reply) else {
        return false;
    };
    if line.len() > MAX_REPLY_BYTES {
        tracing::warn!("a tool reply is larger than the wire allows");
        return false;
    }
    line.push(b'\n');
    writing.write_all(&line).is_ok()
}

/// What one line of an app's connection was.
enum Line {
    /// A request to answer.
    Request(ToolRequest),
    /// A line that does not decode, with what to tell the app.
    Refused(&'static str),
    /// A line past the wire's bound: the app is told so and its connection ends, since the next
    /// byte read is still that line.
    TooLarge,
    /// The app closed its end.
    Done,
}

/// One request line from `reading` .
fn read_request(reading: &mut BufReader<UnixStream>) -> Line {
    let mut line = Vec::new();
    let read = {
        let mut bounded = reading.take((MAX_WIRE_BYTES as u64).saturating_add(1));
        match bounded.read_until(b'\n', &mut line) {
            Ok(read) => read,
            Err(_) => return Line::Done,
        }
    };
    if read == 0 {
        return Line::Done;
    }
    if line.len() > MAX_WIRE_BYTES {
        return Line::TooLarge;
    }
    match serde_json::from_slice::<ToolRequest>(&line) {
        Ok(request) => Line::Request(request),
        Err(error) => {
            tracing::warn!(%error, "a tool request does not decode");
            Line::Refused("the tool request does not decode")
        }
    }
}

/// The app and workspace behind `stream`: the process at the other end,
/// while this session's launch of it still runs.
fn caller_of(inner: &Inner, stream: &UnixStream) -> Option<Caller> {
    let pid = peer_pid(stream)?;
    let app = inner.launches.app_of_descendant(pid)?;
    let workspace = inner.launches.workspace_of(pid, &app)?;
    Some(Caller { app, workspace })
}

/// The pid of the process at the other end of `stream` .
#[cfg(target_os = "linux")]
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd as _;

    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).ok()?;
    // SAFETY: `getsockopt` fills the `ucred` this buffer is sized for and
    // reports in `length` how much it wrote.
    let asked = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut credentials).cast(),
            &raw mut length,
        )
    };
    if asked != 0 || usize::try_from(length).ok() != Some(std::mem::size_of::<libc::ucred>()) {
        return None;
    }
    u32::try_from(credentials.pid).ok()
}

#[cfg(not(target_os = "linux"))]
fn peer_pid(_stream: &UnixStream) -> Option<u32> {
    None
}

/// Answer one request.
fn answer(inner: &Inner, caller: Option<&Caller>, request: &ToolRequest) -> ToolReply {
    if request.protocol != TOOL_PROTOCOL_VERSION {
        return refused("this app was built for another tool protocol");
    }
    if let Err(message) = request.intent.validate() {
        return refused(&message);
    }
    let Some(caller) = caller else {
        return refused("this session did not start the app that asked");
    };
    if request.workspace_id != caller.workspace {
        return refused("the request belongs to another workspace");
    }
    if request.on_behalf_of.is_some() {
        // An open made for a broker invocation is the broker's to admit, and it delivers that open
        // itself; nothing hands an app an invocation to name here yet.
        return refused("an open made for an invocation does not come over this socket");
    }
    match &request.intent {
        ToolIntent::Drives => ToolReply {
            accepted: true,
            message: "listed".into(),
            drives: drives(inner),
            ..ToolReply::default()
        },
        ToolIntent::OpenDrive { drive } => open_drive(inner, caller, drive),
        ToolIntent::Applications => {
            if caller.app != files() {
                return refused("the app list is answered to Files alone");
            }
            ToolReply {
                accepted: true,
                message: "listed".into(),
                applications: applications(inner),
                ..ToolReply::default()
            }
        }
        ToolIntent::DocumentApps { path } => document_apps(inner, caller, path),
        ToolIntent::OpenDocument { path, app } => {
            open_document(inner, caller, path, app.as_deref())
        }
        ToolIntent::OpenApplication { app } => open_application(inner, caller, app),
        ToolIntent::UninstallApplication { app } => uninstall(inner, caller, app),
        ToolIntent::WorkspaceSettings { route } => workspace_settings(inner, caller, *route),
        ToolIntent::IdentityBackup => refused(
            "a portable identity backup is written by setup, on the screens that ask for \
             one; a desk that is already open offers none",
        ),
        // Every other intent opens a package at the typed context it carries.
        _ => open_tool(inner, caller, request),
    }
}

/// The media attached now, as Files lists them.
fn drives(inner: &Inner) -> Vec<DriveEntry> {
    inner
        .now
        .lock_recover()
        .media
        .iter()
        .take(MAX_LISTED_DRIVES)
        .map(|medium| DriveEntry {
            id: drive_id(medium),
            label: medium.label.clone().unwrap_or_default(),
            size_bytes: medium.size,
            // The session announces the media a person plugs in, not the disks the machine was
            // built with.
            internal: false,
            open_at: medium
                .mount
                .exists()
                .then(|| medium.mount.display().to_string()),
        })
        .collect()
}

/// What [`ToolIntent::OpenDrive`] names a medium by: its block device's own name, which the wire's
/// drive ids allow.
fn drive_id(medium: &Medium) -> String {
    medium
        .device
        .rsplit('/')
        .next()
        .unwrap_or(&medium.device)
        .to_owned()
}

/// The person's apps, as the session's lists last read them.
fn applications(inner: &Inner) -> Vec<AppEntry> {
    inner
        .now
        .lock_recover()
        .apps
        .clone()
        .unwrap_or_default()
        .into_iter()
        .take(MAX_LISTED_APPS)
        .collect()
}

/// Open the drive `drive` in Files, where it is mounted: the folder is what the person asked to
/// see.
fn open_drive(inner: &Inner, caller: &Caller, drive: &str) -> ToolReply {
    if caller.app != files() {
        return refused("a drive is opened from Files alone");
    }
    let Some(medium) = inner
        .now
        .lock_recover()
        .media
        .iter()
        .find(|medium| drive_id(medium) == drive)
        .cloned()
    else {
        return refused("no such drive is attached");
    };
    match inner.launches.ask(
        files_on(&medium.mount),
        &caller.workspace,
        LaunchPlace::Focus,
        None,
    ) {
        Ok(()) => accepted("opened"),
        Err(message) => refused(&message),
    }
}

/// The apps that open the document at `path` in the caller's workspace, the one it opens with
/// today first: what Files' "Open with" lists and what opening the document starts.
fn openers(inner: &Inner, caller: &Caller, path: &Path) -> Result<Vec<Launch>, String> {
    let (reply, answered) = std::sync::mpsc::sync_channel(1);
    inner.asks.lock_recover().push(Ask::Openers {
        path: path.to_path_buf(),
        workspace: caller.workspace.clone(),
        reply,
    });
    (inner.wake)();
    match answered.recv_timeout(ANSWER_WAIT) {
        Ok(openers) => Ok(openers),
        Err(_) => Err("this session did not say which apps open the file".into()),
    }
}

/// The apps that open the document at `path` , the one this workspace opens it with today first:
/// what Files' "Open with" lists.
fn document_apps(inner: &Inner, caller: &Caller, path: &str) -> ToolReply {
    if caller.app != files() {
        return refused("the apps that open a document are listed for Files alone");
    }
    let launches = match openers(inner, caller, Path::new(path)) {
        Ok(launches) => launches,
        Err(message) => return refused(&message),
    };
    let lists = crate::app_icons::app_lists().unwrap_or_default();
    let documents = crate::documents::candidates(&launches, &lists)
        .into_iter()
        .take(MAX_LISTED_APPS)
        .map(|mut app| {
            // This thread may read the disk: the menu has the app's own icon the first time it opens.
            app.artwork =
                crate::app_icons::app_png(&app.id, runtime_portal::tools::DOCUMENT_APP_ARTWORK_PX);
            app
        })
        .collect();
    ToolReply {
        accepted: true,
        message: "listed".into(),
        documents,
        ..ToolReply::default()
    }
}

/// Open the document at `path` in `app`, one of the apps that open its type,
/// or in the app this workspace opens the type with today.
fn open_document(inner: &Inner, caller: &Caller, path: &str, app: Option<&str>) -> ToolReply {
    if caller.app != files() {
        return refused("a document is opened from Files alone");
    }
    let launches = match openers(inner, caller, Path::new(path)) {
        Ok(launches) => launches,
        Err(message) => return refused(&message),
    };
    let launch = match app {
        Some(app) => launches
            .into_iter()
            .find(|launch| crate::documents::app_of(launch) == Some(app)),
        None => launches.into_iter().next(),
    };
    let Some(launch) = launch else {
        return refused(if app.is_some() {
            "no such app opens this document"
        } else {
            "nothing on this device opens this document"
        });
    };
    match inner
        .launches
        .ask(launch, &caller.workspace, LaunchPlace::Focus, None)
    {
        Ok(()) => accepted("opened"),
        Err(message) => refused(&message),
    }
}

/// Open an app from the person's list, by a Tilo package id or the machine's own desktop id.
fn open_application(inner: &Inner, caller: &Caller, app: &str) -> ToolReply {
    if caller.app != files() {
        return refused("an app is opened from Files alone");
    }
    let known = inner.now.lock_recover().apps.as_ref().is_some_and(|apps| {
        apps.iter().any(|listed| {
            listed.id == app
                || listed
                    .package
                    .as_deref()
                    .is_some_and(|package| package == app)
        })
    });
    if !known {
        return refused("no such app is listed here");
    }
    let launch = Launch::parse(app)
        .or_else(|| Launch::parse(&format!("{}{app}", tilo_app_launch::launch::HOST_PREFIX)));
    let Some(launch) = launch else {
        return refused("this app has nothing to start");
    };
    match inner
        .launches
        .ask(launch, &caller.workspace, LaunchPlace::Focus, None)
    {
        Ok(()) => accepted("opened"),
        Err(message) => refused(&message),
    }
}

/// Open the package `request` names at the context it asks for: a terminal at a folder, a settings
/// route, a container shell.
fn open_tool(inner: &Inner, caller: &Caller, request: &ToolRequest) -> ToolReply {
    let Some(package) = request.intent.package_id() else {
        return refused("this request opens no app");
    };
    let Some(launch) = Launch::parse(package) else {
        return refused("this request opens no app");
    };
    // The tool reads what it was opened for from its own process: the launcher hands it on as
    // `TILO_TOOL_CONTEXT` .
    let context = serde_json::to_string(&request.intent).ok();
    match inner
        .launches
        .ask(launch, &caller.workspace, LaunchPlace::Focus, context)
    {
        Ok(()) => accepted("opened"),
        Err(message) => refused(&message),
    }
}

/// Ask the person whether to uninstall `app` , in the sheet that owns that question: the loop puts
/// it up on its next pass.
fn uninstall(inner: &Inner, caller: &Caller, app: &str) -> ToolReply {
    if caller.app != files() {
        return refused("an app is uninstalled from Files alone");
    }
    let (reply, answered) = std::sync::mpsc::sync_channel(1);
    inner.asks.lock_recover().push(Ask::Uninstall {
        app: app.to_owned(),
        reply,
    });
    (inner.wake)();
    match answered.recv_timeout(ANSWER_WAIT) {
        Ok(Ok(())) => accepted("asked"),
        Ok(Err(message)) => refused(&message),
        Err(_) => refused("the question could not be put up"),
    }
}

/// Show workspace settings for the caller's workspace, on `route` 's tab: the loop puts the sheet
/// up on its next pass, and only where that workspace is the one in front, so a request never
/// switches what the person sees.
fn workspace_settings(inner: &Inner, caller: &Caller, route: WorkspaceSettingsRoute) -> ToolReply {
    let (reply, answered) = std::sync::mpsc::sync_channel(1);
    inner.asks.lock_recover().push(Ask::WorkspaceSettings {
        workspace: caller.workspace.clone(),
        route,
        reply,
    });
    (inner.wake)();
    match answered.recv_timeout(ANSWER_WAIT) {
        Ok(Ok(())) => accepted("opened"),
        Ok(Err(message)) => refused(&message),
        Err(_) => refused("workspace settings did not open in time"),
    }
}

/// A request that was taken.
fn accepted(message: &str) -> ToolReply {
    ToolReply {
        accepted: true,
        message: message.into(),
        ..ToolReply::default()
    }
}

/// A request that was not taken, and why.
fn refused(message: &str) -> ToolReply {
    ToolReply {
        accepted: false,
        message: message.into(),
        ..ToolReply::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime_portal::tools::{AppKind, AppRemoval};

    fn medium(device: &str) -> Medium {
        Medium {
            device: format!("/org/freedesktop/UDisks2/block_devices/{device}"),
            label: Some("KINGSTON".into()),
            size: 64_000_000_000,
            mount: std::path::PathBuf::from("/run/media/themselves/KINGSTON"),
        }
    }

    #[test]
    fn a_drive_is_named_by_its_block_device_and_is_open_only_when_mounted() {
        let handle = Handle::new(Arc::new(Launches::default()), std::sync::Arc::new(|| {}));
        handle.saw_media(vec![medium("sdb1")]);
        let listed = drives(&handle.inner);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "sdb1");
        assert_eq!(listed[0].label, "KINGSTON");
        assert!(
            listed[0].open_at.is_none(),
            "a mount the machine does not have is not open"
        );
    }

    #[test]
    fn the_apps_that_open_a_document_are_answered_to_files_alone() {
        let handle = Handle::new(Arc::new(Launches::default()), std::sync::Arc::new(|| {}));
        let caller = Caller {
            app: "so.tilo.notes".into(),
            workspace: "ws".into(),
        };
        let reply = document_apps(&handle.inner, &caller, "/home/you/notes.pdf");
        assert!(!reply.accepted);
        assert!(reply.documents.is_empty());
    }

    #[test]
    fn the_app_list_announces_a_change_only_when_it_changes() {
        let handle = Handle::new(Arc::new(Launches::default()), std::sync::Arc::new(|| {}));
        let first = vec![AppEntry {
            id: "so.tilo.notes".into(),
            name: "Notes".into(),
            package: Some("so.tilo.notes".into()),
            icon: None,
            program: None,
            removal: AppRemoval::RemoveFromApps,
            kind: AppKind::Tilo,
            size: None,
        }];
        handle.saw_apps(&first);
        assert_eq!(
            *handle.inner.apps_changed.borrow(),
            2,
            "the first list is a change from none"
        );
        handle.saw_apps(&first);
        assert_eq!(
            *handle.inner.apps_changed.borrow(),
            2,
            "the same list wakes nobody"
        );
        handle.saw_apps(&[]);
        assert_eq!(*handle.inner.apps_changed.borrow(), 3);
    }
}
