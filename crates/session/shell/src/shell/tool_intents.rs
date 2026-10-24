//! Session service for typed, contextual launches from sandboxed system UI.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use os_actions::{TOOL_PROTOCOL_VERSION, ToolIntent, ToolReply, ToolRequest};

use crate::shell::chrome_state::Chrome;
use tilo_desktop::workspace_settings::WsTab;

pub const TOOL_SERVICE: &str = "tools";
const MAX_WIRE_BYTES: u64 = 4 * 1024;
const MAX_PENDING_SHELL_ROUTES: usize = 16;

type Handler = Arc<dyn Fn(ToolRequest) -> Result<(), String> + Send + Sync>;

pub struct ToolServiceHandle {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    path: PathBuf,
    queues: Queues,
}

struct ShellRequest {
    request: ToolRequest,
    reply: mpsc::SyncSender<Result<(), String>>,
}

/// An app's open, waiting for the compositor thread to decide it
/// ([`crate::shell::app_opens`]).
struct OpenRequest {
    request: ToolRequest,
    caller: Option<crate::shell::app_opens::Caller>,
    reply: mpsc::SyncSender<Decision>,
}

/// What the compositor thread decided about an app's open.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Nothing is behind it.
    Refused,
    /// The tools thread launches it in this room once the app is answered.
    Launch(crate::shell::state::RoomId),
    /// The person was shown what became of it; nothing to launch.
    Shown,
}

/// What the client threads hand the compositor thread, and back.
#[derive(Clone, Default)]
struct Queues {
    workspace: Arc<Mutex<Vec<ShellRequest>>>,
    identity_backup: Arc<Mutex<Vec<ShellRequest>>>,
    opens: Arc<Mutex<Vec<OpenRequest>>>,
    /// Launches an app's open started that failed, for the person.
    failed: Arc<Mutex<Vec<crate::shell::app_opens::FailedOpen>>>,
}

impl Drop for ToolServiceHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // The listener sleeps in `accept`; one connection wakes it to see the
        // flag. If nothing can connect it is not waited for.
        if let Some(thread) = self.thread.take()
            && UnixStream::connect(&self.path).is_ok()
        {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// # Errors
///
/// Returns the OS error if the services root can't be created, the
/// socket path is already bound by a live listener, or binding the new
/// listener fails.
pub fn spawn() -> std::io::Result<ToolServiceHandle> {
    launcher::services::ensure_services_root()?;
    let path = launcher::services::socket_for(TOOL_SERVICE);
    let queues = Queues::default();
    let pending_workspace = Arc::clone(&queues.workspace);
    let pending_identity_backup = Arc::clone(&queues.identity_backup);
    spawn_at(
        path,
        Arc::new(move |request| {
            crate::shell::state::RoomId::parse(&request.workspace_id)
                .ok_or_else(|| "invalid workspace identity".to_string())?;
            request.intent.validate()?;
            let pending = match route(&request.intent) {
                Some(Route::WorkspaceSettings) => &pending_workspace,
                Some(Route::IdentityBackup) => &pending_identity_backup,
                None => return Err("not a shell route".into()),
            };
            {
                let (reply, result) = mpsc::sync_channel(1);
                let mut routes = pending
                    .lock()
                    .map_err(|_| "shell route queue is unavailable".to_string())?;
                if routes.len() >= MAX_PENDING_SHELL_ROUTES {
                    return Err("too many shell routes are pending".into());
                }
                routes.push(ShellRequest { request, reply });
                drop(routes);
                result
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| "the shell did not accept the requested route".to_string())?
            }
        }),
        queues,
    )
}

/// Which shell surface a validated intent opens, on the compositor thread.
/// Opening an app or a tool is not a route: it is decided as an app's open
/// ([`crate::shell::app_opens`]).
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// Workspace Settings chrome.
    WorkspaceSettings,
    /// The portable identity backup flow.
    IdentityBackup,
}

fn route(intent: &ToolIntent) -> Option<Route> {
    match intent {
        ToolIntent::WorkspaceSettings { .. } => Some(Route::WorkspaceSettings),
        ToolIntent::IdentityBackup => Some(Route::IdentityBackup),
        _ => None,
    }
}

fn spawn_at(path: PathBuf, handler: Handler, queues: Queues) -> std::io::Result<ToolServiceHandle> {
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("{} already has a live listener", path.display()),
            ));
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let worker_shutdown = Arc::clone(&shutdown);
    let served = queues.clone();
    // Launches a client asks for report to the session that started the
    // service, like every other launch.
    let thread = crate::shell::launcher::spawn_reporting(
        std::thread::Builder::new().name("tilo-tool-intents".into()),
        move || {
            loop {
                match listener.accept() {
                    Ok(_) if worker_shutdown.load(Ordering::Acquire) => break,
                    Ok((stream, _)) => {
                        let handler = Arc::clone(&handler);
                        let queues = served.clone();
                        let _ = crate::shell::launcher::spawn_reporting(
                            std::thread::Builder::new().name("tilo-tool-client".into()),
                            move || serve_client(stream, &handler, &queues),
                        );
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "tool-intent listener failed");
                        break;
                    }
                }
            }
        },
    )?;
    Ok(ToolServiceHandle {
        shutdown,
        thread: Some(thread),
        path,
        queues,
    })
}

impl ToolServiceHandle {
    /// Apply queued workspace routes on the compositor thread, where the
    /// non-`Send` chrome model lives. A request is accepted only for the
    /// workspace currently containing the caller; it never switches context
    /// behind the person's back.
    pub fn pump_workspace_settings(&self, chrome: &Chrome) {
        let requests = self
            .queues
            .workspace
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        for pending in requests {
            let result = apply_workspace_settings(chrome, &pending.request);
            let _ = pending.reply.send(result);
        }
    }

    /// Open trusted portable-backup chrome on the compositor thread. Like
    /// workspace Settings routes, it is bound to the caller's active room.
    pub fn pump_identity_backup<F>(&self, chrome: &Chrome, mut open: F)
    where
        F: FnMut() -> Result<(), String>,
    {
        let requests = self
            .queues
            .identity_backup
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        for pending in requests {
            let result = apply_identity_backup(chrome, &pending.request, &mut open);
            let _ = pending.reply.send(result);
        }
    }

    /// Decide the apps' opens waiting for the compositor thread, and tell
    /// the person of the launches they started that failed. An open is the
    /// caller's to make when the person's press in one of its windows is
    /// behind it, or `broker` admits it for the invocation it names.
    pub fn pump_app_opens(
        &self,
        chrome: &Chrome,
        broker: Option<&tilo_action_broker::collab_broker::AppActionBroker>,
    ) {
        let failed = self
            .queues
            .failed
            .lock()
            .map(|mut failed| std::mem::take(&mut *failed))
            .unwrap_or_default();
        crate::shell::app_opens::notify_failed(chrome, failed);
        let opens = self
            .queues
            .opens
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        for open in opens {
            let decision = decide_open(chrome, broker, &open);
            let _ = open.reply.send(decision);
        }
    }
}

/// Decide one app's open on the compositor thread
/// ([`crate::shell::app_opens`]): refused unless the person's press or an
/// admitted invocation is behind it; else a launch, or what the person is
/// shown instead.
fn decide_open(
    chrome: &Chrome,
    broker: Option<&tilo_action_broker::collab_broker::AppActionBroker>,
    open: &OpenRequest,
) -> Decision {
    use crate::shell::app_opens;
    // Only an app the shell launched opens anything.
    let (Some(caller), Some(room)) = (
        open.caller.as_ref(),
        crate::shell::state::RoomId::parse(&open.request.workspace_id),
    ) else {
        return Decision::Refused;
    };
    let Some(target) = app_opens::target(&open.request.intent) else {
        return Decision::Refused;
    };
    let authority = app_opens::authorize(
        Some(caller),
        &open.request,
        &target,
        |package| crate::compositor::activation::person_initiated(&chrome.borrow().tree(), package),
        |caller, invocation, target| {
            broker
                .ok_or_else(|| "no action broker".to_string())?
                .admit_app_open(&caller.package, &caller.workspace, invocation, target)
        },
    );
    if authority.is_none() {
        return Decision::Refused;
    }
    let resolution = app_opens::resolve(
        &open.request.intent,
        app_opens::installed,
        crate::shell::catalogue::available_apps,
    );
    if resolution == app_opens::Resolution::Launch {
        return Decision::Launch(room);
    }
    app_opens::show(
        chrome,
        caller,
        &app_opens::opened_app(&open.request.intent),
        &resolution,
    );
    Decision::Shown
}

fn apply_workspace_settings(chrome: &Chrome, request: &ToolRequest) -> Result<(), String> {
    let ToolIntent::WorkspaceSettings { route } = &request.intent else {
        return Err("not a workspace settings route".into());
    };
    ensure_active_workspace_matches(chrome, request)?;
    let mut state = chrome.borrow_mut();
    let tab = match route {
        os_actions::WorkspaceSettingsRoute::General => WsTab::General,
        os_actions::WorkspaceSettingsRoute::Members => WsTab::Members,
        os_actions::WorkspaceSettingsRoute::PrivacySecurity => WsTab::Permissions,
        os_actions::WorkspaceSettingsRoute::AppsFiles => WsTab::Apps,
    };
    state.open_workspace_settings(tab);
    state.bump();
    Ok(())
}

fn apply_identity_backup<F>(
    chrome: &Chrome,
    request: &ToolRequest,
    open: &mut F,
) -> Result<(), String>
where
    F: FnMut() -> Result<(), String>,
{
    if !matches!(&request.intent, ToolIntent::IdentityBackup) {
        return Err("not an identity backup route".into());
    }
    ensure_active_workspace_matches(chrome, request)?;
    open()
}

fn ensure_active_workspace_matches(chrome: &Chrome, request: &ToolRequest) -> Result<(), String> {
    let state = chrome.borrow();
    let active_room = state
        .tree()
        .desktop
        .active_workspace()
        .map(|workspace| workspace.room().to_string())
        .ok_or_else(|| "there is no active workspace".to_string())?;
    if active_room != request.workspace_id {
        return Err("Personal Settings is attached to a different workspace".into());
    }
    Ok(())
}

fn serve_client(stream: UnixStream, handler: &Handler, queues: &Queues) {
    // Darwin inherits the listener's nonblocking flag on accept. This worker
    // uses blocking framed I/O; an empty receive is not the end of a session.
    if let Err(error) = stream.set_nonblocking(false) {
        tracing::warn!(%error, "cannot configure blocking service connection");
        return;
    }
    // The app on the other end, for the one request whose answer depends on
    // it: only Files, which asks when the person clicks a drive, opens a
    // remembered drive without the person being asked again.
    #[cfg(target_os = "linux")]
    let pid = crate::compositor::native::wayland_peer_credentials(&stream)
        .and_then(|credentials| u32::try_from(credentials.pid).ok());
    #[cfg(target_os = "linux")]
    let caller = pid.and_then(crate::shell::launcher::verified_package_for_descendant);
    #[cfg(not(target_os = "linux"))]
    let caller: Option<String> = None;
    // The app and workspace its opens are made from, as its launch recorded
    // them: never what the request says.
    #[cfg(target_os = "linux")]
    let opener = pid
        .and_then(crate::shell::launcher::verified_launch_for_descendant)
        .and_then(|identity| {
            Some(crate::shell::app_opens::Caller {
                workspace: identity.workspace_id?,
                package: identity.package_id,
            })
        });
    #[cfg(not(target_os = "linux"))]
    let opener: Option<crate::shell::app_opens::Caller> = None;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        let Ok(read) = reader
            .by_ref()
            .take(MAX_WIRE_BYTES + 1)
            .read_line(&mut line)
        else {
            return;
        };
        if read == 0 {
            return;
        }
        let mut then: Option<Box<dyn FnOnce()>> = None;
        let reply = if read as u64 > MAX_WIRE_BYTES {
            ToolReply {
                accepted: false,
                message: "tool request is too large".into(),
                ..ToolReply::default()
            }
        } else {
            match serde_json::from_str::<ToolRequest>(&line) {
                Ok(request)
                    if request.protocol == TOOL_PROTOCOL_VERSION
                        && crate::shell::app_opens::opens_app(&request.intent) =>
                {
                    let (reply, launch) = open_reply(request, opener.clone(), queues);
                    then = launch;
                    reply
                }
                Ok(request) if request.protocol == TOOL_PROTOCOL_VERSION => {
                    if let Some(reply) = drive_reply(&request, caller.as_deref())
                        .or_else(|| app_reply(&request, caller.as_deref()))
                    {
                        reply
                    } else {
                        match handler(request) {
                            Ok(()) => ToolReply {
                                accepted: true,
                                message: "opened".into(),
                                ..ToolReply::default()
                            },
                            Err(message) => ToolReply {
                                accepted: false,
                                message,
                                ..ToolReply::default()
                            },
                        }
                    }
                }
                Ok(_) => ToolReply {
                    accepted: false,
                    message: "unsupported tool protocol".into(),
                    ..ToolReply::default()
                },
                Err(_) => ToolReply {
                    accepted: false,
                    message: "invalid tool request".into(),
                    ..ToolReply::default()
                },
            }
        };
        let Ok(mut bytes) = serde_json::to_vec(&reply) else {
            return;
        };
        bytes.push(b'\n');
        if reader.get_mut().write_all(&bytes).is_err() {
            return;
        }
        // The app has its answer; the launch it asked for runs now.
        if let Some(then) = then {
            then();
        }
    }
}

/// Answer an app's open ([`crate::shell::app_opens`]): the compositor thread
/// decides it, the app is told only whether it was taken, and a launch it
/// was taken for is returned to run once the app has that answer.
fn open_reply(
    request: ToolRequest,
    caller: Option<crate::shell::app_opens::Caller>,
    queues: &Queues,
) -> (ToolReply, Option<Box<dyn FnOnce()>>) {
    use crate::shell::app_opens::{FailedOpen, REFUSED, REQUESTED};
    let refused = |message: String| ToolReply {
        accepted: false,
        message,
        ..ToolReply::default()
    };
    if let Err(message) = request.intent.validate() {
        return (refused(message), None);
    }
    let intent = request.intent.clone();
    let (reply, decided) = mpsc::sync_channel(1);
    {
        let Ok(mut opens) = queues.opens.lock() else {
            return (refused(REFUSED.into()), None);
        };
        if opens.len() >= MAX_PENDING_SHELL_ROUTES {
            return (refused("too many opens are pending".into()), None);
        }
        opens.push(OpenRequest {
            request,
            caller,
            reply,
        });
    }
    let requested = ToolReply {
        accepted: true,
        message: REQUESTED.into(),
        ..ToolReply::default()
    };
    match decided.recv_timeout(Duration::from_secs(2)) {
        Ok(Decision::Launch(room)) => {
            let failed = Arc::clone(&queues.failed);
            let launch = move || {
                if let Err(reason) = crate::shell::app_opens::launch(&intent, room)
                    && let Ok(mut failed) = failed.lock()
                {
                    failed.push(FailedOpen {
                        app: crate::shell::app_opens::opened_app(&intent),
                        reason,
                    });
                }
            };
            (requested, Some(Box::new(launch)))
        }
        Ok(Decision::Shown) => (requested, None),
        Ok(Decision::Refused) | Err(_) => (refused(REFUSED.into()), None),
    }
}

/// The NTFS drive requests, answered from the session's drive worker
/// ([`crate::shell::drives`]); `None` for every other intent.
fn drive_reply(request: &ToolRequest, caller: Option<&str>) -> Option<ToolReply> {
    let answer = match &request.intent {
        ToolIntent::Drives => Ok(drive_list()),
        ToolIntent::OpenDrive { drive } => request
            .intent
            .validate()
            .and_then(|()| open_drive(drive, caller))
            .map(|()| Vec::new()),
        _ => return None,
    };
    Some(match answer {
        Ok(drives) => ToolReply {
            accepted: true,
            message: "opened".into(),
            drives,
            ..ToolReply::default()
        },
        Err(message) => ToolReply {
            accepted: false,
            message,
            ..ToolReply::default()
        },
    })
}

/// The person's apps, for Files' Applications place, and the requests made
/// from it and from the Store ([`crate::shell::app_list`]); `None` for every
/// other intent. The list is the person's own view of their apps, so only
/// Files is answered it; an uninstall is asked of the person in the shell's
/// sheet whoever requests it, and only Files and the Store may request one.
fn app_reply(request: &ToolRequest, caller: Option<&str>) -> Option<ToolReply> {
    use crate::shell::search::providers::applications::SYSTEM_FILE_MANAGER;
    const STORE: &str = "so.tilo.store";
    let from_files = caller == Some(SYSTEM_FILE_MANAGER);
    let answer = match &request.intent {
        ToolIntent::Applications if from_files => Ok(crate::shell::app_list::listed()
            .iter()
            .map(crate::shell::app_list::AppListItem::entry)
            .filter(os_actions::AppEntry::fits)
            .take(os_actions::MAX_LISTED_APPS)
            .collect()),
        ToolIntent::UninstallApplication { app } if from_files || caller == Some(STORE) => {
            request.intent.validate().map(|()| {
                crate::shell::app_list::ask(app);
                Vec::new()
            })
        }
        ToolIntent::Applications | ToolIntent::UninstallApplication { .. } => {
            Err("not available to this app".to_string())
        }
        _ => return None,
    };
    Some(match answer {
        Ok(applications) => ToolReply {
            accepted: true,
            message: "opened".into(),
            applications,
            ..ToolReply::default()
        },
        Err(message) => ToolReply {
            accepted: false,
            message,
            ..ToolReply::default()
        },
    })
}

#[cfg(target_os = "linux")]
fn drive_list() -> Vec<os_actions::DriveEntry> {
    crate::shell::drives::listed()
}

#[cfg(not(target_os = "linux"))]
fn drive_list() -> Vec<os_actions::DriveEntry> {
    Vec::new()
}

#[cfg(target_os = "linux")]
fn open_drive(drive: &str, caller: Option<&str>) -> Result<(), String> {
    let from_files =
        caller == Some(crate::shell::search::providers::applications::SYSTEM_FILE_MANAGER);
    crate::shell::drives::request_open(drive, from_files)
}

#[cfg(not(target_os = "linux"))]
fn open_drive(_drive: &str, _caller: Option<&str>) -> Result<(), String> {
    Err("drives are not available here".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use os_actions::{ContainerEngine, ToolIntent, VpnRoute, WorkspaceSettingsRoute};

    #[cfg(target_os = "linux")]
    #[test]
    fn idle_listener_sleeps_until_a_client_arrives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tools.sock");
        let (service, listener) = tilo_test_env::idle::spawned_thread("tilo-tool-inten", || {
            spawn_at(path.clone(), Arc::new(|_| Ok(())), Queues::default()).unwrap()
        });
        tilo_test_env::idle::assert_stays_asleep(&listener, "tool-intent listener");
        UnixStream::connect(&path).expect("still accepts clients");
        let (dropped, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(service);
            let _ = dropped.send(());
        });
        tilo_test_env::recv("dropping the service to wake its listener", &done);
        assert!(!path.exists(), "drop unlinks the socket");
    }

    // An app opens another, or a system tool, only for the person's press
    // in it or an invocation delivered to it. At the door, a request from a
    // process the shell did not launch has neither: it is refused, the same
    // for a tool and for an app, and nothing is launched for it.
    #[test]
    fn an_open_nothing_is_behind_is_refused_at_the_door() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tools.sock");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        let service = spawn_at(
            path.clone(),
            Arc::new(move |r| {
                record.lock().unwrap().push(r.intent);
                Ok(())
            }),
            Queues::default(),
        )
        .unwrap();
        let chrome = crate::shell::chrome_state::ChromeState::new(
            crate::shell::state::WorkspaceTree::bootstrap_demo(),
        );
        let room = chrome
            .borrow()
            .tree()
            .desktop
            .active_workspace()
            .unwrap()
            .room()
            .to_string();
        let ask = |intent: ToolIntent| {
            let path = path.clone();
            let room = room.clone();
            std::thread::spawn(move || {
                let mut stream = UnixStream::connect(path).unwrap();
                let request = ToolRequest {
                    protocol: TOOL_PROTOCOL_VERSION,
                    on_behalf_of: None,
                    workspace_id: room,
                    intent,
                };
                serde_json::to_writer(&mut stream, &request).unwrap();
                stream.write_all(b"\n").unwrap();
                let mut response = String::new();
                BufReader::new(stream).read_line(&mut response).unwrap();
                serde_json::from_str::<ToolReply>(&response).unwrap()
            })
        };
        let answer = |client: std::thread::JoinHandle<ToolReply>| {
            while !client.is_finished() {
                service.pump_app_opens(&chrome, None);
                std::thread::yield_now();
            }
            client.join().unwrap()
        };

        let tool = answer(ask(ToolIntent::Vpn {
            route: VpnRoute::Profile,
            profile: Some("Work VPN".into()),
        }));
        let app = answer(ask(ToolIntent::OpenApplication {
            app: "so.tilo.contacts".into(),
        }));

        assert!(!tool.accepted, "{tool:?}");
        assert_eq!(tool.message, crate::shell::app_opens::REFUSED);
        assert_eq!(app, tool, "an app is refused exactly as a tool is");
        assert!(seen.lock().unwrap().is_empty(), "nothing is launched");
    }

    #[test]
    fn workspace_tool_intents_are_typed_and_confined_to_the_active_room() {
        {
            let chrome = crate::shell::chrome_state::ChromeState::new(
                crate::shell::state::WorkspaceTree::bootstrap_demo(),
            );
            let room = chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .unwrap()
                .room()
                .to_string();
            let request = ToolRequest {
                protocol: TOOL_PROTOCOL_VERSION,
                on_behalf_of: None,
                workspace_id: room,
                intent: ToolIntent::WorkspaceSettings {
                    route: WorkspaceSettingsRoute::PrivacySecurity,
                },
            };

            apply_workspace_settings(&chrome, &request).unwrap();

            let state = chrome.borrow();
            assert!(state.overlays.ws_settings);
            assert_eq!(state.ws_settings_tab, WsTab::Permissions);
        }

        {
            let chrome = crate::shell::chrome_state::ChromeState::new(
                crate::shell::state::WorkspaceTree::bootstrap_demo(),
            );
            let request = ToolRequest {
                protocol: TOOL_PROTOCOL_VERSION,
                on_behalf_of: None,
                workspace_id: "7690fd4b-55aa-4a4f-8339-7405f4ad18ac".into(),
                intent: ToolIntent::WorkspaceSettings {
                    route: WorkspaceSettingsRoute::Members,
                },
            };

            assert!(apply_workspace_settings(&chrome, &request).is_err());
            let state = chrome.borrow();
            assert!(!state.overlays.ws_settings);
            assert_eq!(state.ws_settings_tab, WsTab::General);
        }
    }

    // Opening Terminal, a container shell, a system tool or an app is an
    // app's open, decided as one; the shell's own surfaces are routes.
    #[test]
    fn opens_are_decided_as_opens_and_shell_surfaces_are_routes() {
        use crate::shell::app_opens::opens_app;
        for open in [
            ToolIntent::Terminal {
                directory: Some("/home/u/Projects".into()),
            },
            ToolIntent::Terminal { directory: None },
            ToolIntent::ContainerShell {
                container: "web".into(),
                engine: ContainerEngine::Distrobox,
            },
            ToolIntent::Vpn {
                route: VpnRoute::Overview,
                profile: None,
            },
            ToolIntent::OpenApplication {
                app: "so.tilo.contacts".into(),
            },
        ] {
            assert!(opens_app(&open), "{open:?}");
            assert_eq!(route(&open), None, "{open:?}");
        }
        let settings = ToolIntent::WorkspaceSettings {
            route: WorkspaceSettingsRoute::General,
        };
        assert!(!opens_app(&settings));
        assert_eq!(route(&settings), Some(Route::WorkspaceSettings));
        assert!(!opens_app(&ToolIntent::IdentityBackup));
        assert_eq!(
            route(&ToolIntent::IdentityBackup),
            Some(Route::IdentityBackup)
        );
        assert!(!opens_app(&ToolIntent::OpenDrive {
            drive: "sda1".into()
        }));
        assert!(!opens_app(&ToolIntent::UninstallApplication {
            app: "so.tilo.contacts".into()
        }));
    }

    #[test]
    fn identity_backup_intents_open_the_trusted_flow_only_for_this_workspace() {
        {
            let chrome = crate::shell::chrome_state::ChromeState::new(
                crate::shell::state::WorkspaceTree::bootstrap_demo(),
            );
            let room = chrome
                .borrow()
                .tree()
                .desktop
                .active_workspace()
                .unwrap()
                .room()
                .to_string();
            let request = ToolRequest {
                protocol: TOOL_PROTOCOL_VERSION,
                on_behalf_of: None,
                workspace_id: room,
                intent: ToolIntent::IdentityBackup,
            };
            let mut opens = 0;

            apply_identity_backup(&chrome, &request, &mut || {
                opens += 1;
                Ok(())
            })
            .unwrap();

            assert_eq!(opens, 1);
        }

        {
            let chrome = crate::shell::chrome_state::ChromeState::new(
                crate::shell::state::WorkspaceTree::bootstrap_demo(),
            );
            let request = ToolRequest {
                protocol: TOOL_PROTOCOL_VERSION,
                on_behalf_of: None,
                workspace_id: "7690fd4b-55aa-4a4f-8339-7405f4ad18ac".into(),
                intent: ToolIntent::IdentityBackup,
            };
            let mut opens = 0;

            assert!(
                apply_identity_backup(&chrome, &request, &mut || {
                    opens += 1;
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(opens, 0);
        }
    }
}
