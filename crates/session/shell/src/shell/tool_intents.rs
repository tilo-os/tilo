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
    workspace_requests: Arc<Mutex<Vec<ShellRequest>>>,
    identity_backup_requests: Arc<Mutex<Vec<ShellRequest>>>,
}

struct ShellRequest {
    request: ToolRequest,
    reply: mpsc::SyncSender<Result<(), String>>,
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
    let workspace_requests = Arc::new(Mutex::new(Vec::new()));
    let pending_workspace = Arc::clone(&workspace_requests);
    let identity_backup_requests = Arc::new(Mutex::new(Vec::new()));
    let pending_identity_backup = Arc::clone(&identity_backup_requests);
    spawn_at(
        path,
        Arc::new(move |request| {
            let room = crate::shell::state::RoomId::parse(&request.workspace_id)
                .ok_or_else(|| "invalid workspace identity".to_string())?;
            request.intent.validate()?;
            let pending = match route(&request.intent) {
                Route::WorkspaceSettings => &pending_workspace,
                Route::IdentityBackup => &pending_identity_backup,
                Route::TerminalCommand(argv) => {
                    return crate::shell::launcher::launch_terminal_command(&argv, room)
                        .map(|_| ())
                        .map_err(|e| e.user_message());
                }
                Route::Package => {
                    return crate::shell::launcher::launch_tool_intent(&request.intent, room)
                        .map(|_| ())
                        .map_err(|e| e.user_message());
                }
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
        workspace_requests,
        identity_backup_requests,
    )
}

/// Where a validated intent is carried out.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    /// Workspace Settings chrome, on the compositor thread.
    WorkspaceSettings,
    /// The portable identity backup flow, on the compositor thread.
    IdentityBackup,
    /// A command run inside Terminal's confined guest.
    TerminalCommand(Vec<String>),
    /// The intent's installed package, given the intent as its context.
    Package,
}

fn route(intent: &ToolIntent) -> Route {
    match intent {
        ToolIntent::WorkspaceSettings { .. } => Route::WorkspaceSettings,
        ToolIntent::IdentityBackup => Route::IdentityBackup,
        _ => intent
            .terminal_argv()
            .map_or(Route::Package, Route::TerminalCommand),
    }
}

fn spawn_at(
    path: PathBuf,
    handler: Handler,
    workspace_requests: Arc<Mutex<Vec<ShellRequest>>>,
    identity_backup_requests: Arc<Mutex<Vec<ShellRequest>>>,
) -> std::io::Result<ToolServiceHandle> {
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
                        let _ = crate::shell::launcher::spawn_reporting(
                            std::thread::Builder::new().name("tilo-tool-client".into()),
                            move || serve_client(stream, &handler),
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
        workspace_requests,
        identity_backup_requests,
    })
}

impl ToolServiceHandle {
    /// Apply queued workspace routes on the compositor thread, where the
    /// non-`Send` chrome model lives. A request is accepted only for the
    /// workspace currently containing the caller; it never switches context
    /// behind the person's back.
    pub fn pump_workspace_settings(&self, chrome: &Chrome) {
        let requests = self
            .workspace_requests
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
            .identity_backup_requests
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        for pending in requests {
            let result = apply_identity_backup(chrome, &pending.request, &mut open);
            let _ = pending.reply.send(result);
        }
    }
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
        .map(|workspace| workspace.room.to_string())
        .ok_or_else(|| "there is no active workspace".to_string())?;
    if active_room != request.workspace_id {
        return Err("Personal Settings is attached to a different workspace".into());
    }
    Ok(())
}

fn serve_client(stream: UnixStream, handler: &Handler) {
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
    let caller = crate::compositor::native::wayland_peer_credentials(&stream)
        .and_then(|credentials| u32::try_from(credentials.pid).ok())
        .and_then(crate::shell::launcher::verified_package_for_descendant);
    #[cfg(not(target_os = "linux"))]
    let caller: Option<String> = None;
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
        let reply = if read as u64 > MAX_WIRE_BYTES {
            ToolReply {
                accepted: false,
                message: "tool request is too large".into(),
                ..ToolReply::default()
            }
        } else {
            match serde_json::from_str::<ToolRequest>(&line) {
                Ok(request) if request.protocol == TOOL_PROTOCOL_VERSION => {
                    if let Some(reply) = drive_reply(&request, caller.as_deref()) {
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
            spawn_at(
                path.clone(),
                Arc::new(|_| Ok(())),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(Mutex::new(Vec::new())),
            )
            .unwrap()
        });
        tilo_test_env::idle::assert_stays_asleep(&listener, "tool-intent listener");
        UnixStream::connect(&path).expect("still accepts clients");
        let started = std::time::Instant::now();
        drop(service);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "drop waited on its listener"
        );
        assert!(!path.exists(), "drop unlinks the socket");
    }

    #[test]
    fn workspace_tool_intents_are_typed_and_confined_to_the_active_room() {
        // Phase `service_accepts_only_versioned_typed_requests`.
        {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("tools.sock");
            let seen = Arc::new(Mutex::new(Vec::new()));
            let record = Arc::clone(&seen);
            let _service = spawn_at(
                path.clone(),
                Arc::new(move |r| {
                    record.lock().unwrap().push(r.intent);
                    Ok(())
                }),
                Arc::new(Mutex::new(Vec::new())),
                Arc::new(Mutex::new(Vec::new())),
            )
            .unwrap();

            let mut stream = UnixStream::connect(path).unwrap();
            let request = ToolRequest {
                protocol: TOOL_PROTOCOL_VERSION,
                workspace_id: "7690fd4b-55aa-4a4f-8339-7405f4ad18ac".into(),
                intent: ToolIntent::Vpn {
                    route: VpnRoute::Profile,
                    profile: Some("Work VPN".into()),
                },
            };
            serde_json::to_writer(&mut stream, &request).unwrap();
            stream.write_all(b"\n").unwrap();
            let mut response = String::new();
            BufReader::new(stream).read_line(&mut response).unwrap();
            assert!(
                serde_json::from_str::<ToolReply>(&response)
                    .unwrap()
                    .accepted
            );
            assert_eq!(seen.lock().unwrap().len(), 1);
        }

        // Phase `workspace_route_opens_exact_tab_for_active_room`.
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
                .room
                .to_string();
            let request = ToolRequest {
                protocol: TOOL_PROTOCOL_VERSION,
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

        // Phase `workspace_route_cannot_switch_or_target_another_room`.
        {
            let chrome = crate::shell::chrome_state::ChromeState::new(
                crate::shell::state::WorkspaceTree::bootstrap_demo(),
            );
            let request = ToolRequest {
                protocol: TOOL_PROTOCOL_VERSION,
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

    #[test]
    fn terminal_intents_run_their_command_in_terminal_or_open_it_plainly() {
        // Phase `files_directory_runs_a_login_shell_there`.
        {
            let intent = ToolIntent::Terminal {
                directory: Some("/home/u/Projects".into()),
            };
            let Route::TerminalCommand(argv) = route(&intent) else {
                panic!("a directory runs a command in Terminal");
            };
            assert_eq!(argv.last().map(String::as_str), Some("/home/u/Projects"));
        }
        // Phase `container_shell_runs_the_engine_cli`.
        {
            let intent = ToolIntent::ContainerShell {
                container: "web".into(),
                engine: ContainerEngine::Distrobox,
            };
            assert_eq!(
                route(&intent),
                Route::TerminalCommand(vec!["distrobox".into(), "enter".into(), "web".into()])
            );
        }
        // Phase `plain_terminal_and_tools_open_their_package`.
        {
            assert_eq!(
                route(&ToolIntent::Terminal { directory: None }),
                Route::Package
            );
            assert_eq!(
                route(&ToolIntent::Vpn {
                    route: VpnRoute::Overview,
                    profile: None,
                }),
                Route::Package
            );
            assert_eq!(
                route(&ToolIntent::WorkspaceSettings {
                    route: WorkspaceSettingsRoute::General,
                }),
                Route::WorkspaceSettings
            );
            assert_eq!(route(&ToolIntent::IdentityBackup), Route::IdentityBackup);
        }
    }

    #[test]
    fn identity_backup_intents_open_the_trusted_flow_only_for_this_workspace() {
        // Phase `identity_backup_route_opens_trusted_flow_for_current_workspace`.
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
                .room
                .to_string();
            let request = ToolRequest {
                protocol: TOOL_PROTOCOL_VERSION,
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

        // Phase `identity_backup_route_cannot_target_another_workspace`.
        {
            let chrome = crate::shell::chrome_state::ChromeState::new(
                crate::shell::state::WorkspaceTree::bootstrap_demo(),
            );
            let request = ToolRequest {
                protocol: TOOL_PROTOCOL_VERSION,
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
