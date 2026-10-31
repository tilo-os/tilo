//! Tabs of one app, opened and closed by the app itself.
//!
//! Applications have no tab strips: a second page, file or conversation is a
//! second window of the app in a tab of the desktop's window group. An app
//! asks for that over its authenticated `shell-ui` connection
//! ([`ui::shell_tabs`]): another window right after its own tab, its own tab
//! closed, the last closed tab back, its group stepped on, or its next window
//! placed beside its own. The connection vouches for the app and the room it
//! runs in; an app can never place, close or step another app's tab, and a
//! request refused changes nothing.
//!
//! The socket is served here, and the asks become desk commands on the
//! session loop's own pass: the desk is never read from another thread.
//!
//! The same connections are made for a window the session opens in an app's
//! running process ([`tilo_app_launch::Connects`]): each window reaches the
//! collaboration broker and the shell-ui socket as itself, so what it asks is
//! answered for its own tab.

use std::io::{BufReader, Write};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use launcher::services::{SHELL_UI_LAUNCH_PROTOCOL, SHELL_UI_SERVICE};
use serde::Deserialize;
use tilo_app_launch::routing;
use tilo_app_launch::{Connects, Launch, Open, WindowConnections};
use tilo_desktop_policy::DesktopPolicy;
use tilo_surface_host::DesktopHost;
use ui::floating_surface::{
    FLOATING_SURFACE_PROTOCOL_VERSION, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
};

use crate::Launches;
use crate::mount::apps::Apps;

/// The longest frame an app may send: a paint stream and a marker
/// inventory both fit, and nothing else comes close.
const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// The longest address an app may ask to open in a new tab: as long as the
/// shell's own link opening takes.
const MAX_TARGET_BYTES: usize = 8 * 1024;

/// How the session makes a window's own connections to its services: the
/// collaboration broker and the shell-ui socket, each of the window's own, so
/// what the window asks is answered for it alone.
pub struct ServiceLinks {
    launches: Arc<Launches>,
}

impl ServiceLinks {
    /// The connections of a window of the apps whose launches `launches`
    /// records.
    #[must_use]
    pub fn new(launches: Arc<Launches>) -> Self {
        Self { launches }
    }

    /// The window's collaboration connection, attested as that window: the
    /// record is the session's own, spent by the connection that names the
    /// window's instance.
    fn collaboration(
        &self,
        app: &str,
        room: &str,
        instance: u64,
        launch: Option<u32>,
    ) -> Option<OwnedFd> {
        let mut stream = UnixStream::connect(launcher::services::collab_socket())
            .inspect_err(|error| {
                tracing::warn!(%error, %app, "a window could not reach the collaboration broker");
            })
            .ok()?;
        self.launches.window(app, room, instance, launch);
        if let Err(error) =
            launcher::services::write_collab_window_handoff(&mut stream, app, room, instance)
        {
            self.launches.window_withdrawn(app, room, instance);
            tracing::warn!(%error, %app, "a window's collaboration connection was not named");
            return None;
        }
        Some(OwnedFd::from(stream))
    }

    /// The window's shell-ui connection, attributed to the launch whose
    /// process `launcher` is: the app and the room it runs in, without which
    /// what it asks reaches no desk.
    fn shell_ui(&self, app: &str, launcher: Option<u32>) -> Option<OwnedFd> {
        let launcher = launcher?;
        self.launches.window_shell_ui(app, launcher);
        let connected = (|| {
            let mut stream = UnixStream::connect(launcher::services::socket_for(SHELL_UI_SERVICE))?;
            let mut frame = serde_json::to_vec(&serde_json::json!({
                "protocol": SHELL_UI_LAUNCH_PROTOCOL,
                "app_id": app,
                // What the app may know of the network was decided for its
                // launch; the window does not widen it.
                "network_status_allowed": false,
            }))
            .map_err(std::io::Error::other)?;
            frame.push(b'\n');
            stream.write_all(&frame)?;
            stream.flush()?;
            Ok::<_, std::io::Error>(stream)
        })();
        match connected {
            Ok(stream) => Some(OwnedFd::from(stream)),
            Err(error) => {
                self.launches.take_window_shell_ui(app);
                tracing::warn!(%error, %app, "a window could not reach the shell-ui service");
                None
            }
        }
    }
}

impl Connects for ServiceLinks {
    fn window(
        &self,
        app: &str,
        room: &str,
        instance: u64,
        launcher_pid: Option<u32>,
    ) -> WindowConnections {
        WindowConnections {
            collaboration: self.collaboration(app, room, instance, launcher_pid),
            shell_ui: self.shell_ui(app, launcher_pid),
        }
    }
}

/// The shell-ui socket this session serves: what an app asks about its own
/// tabs arrives here, and the session's loop answers it.
pub struct AppTabs {
    requests: mpsc::Receiver<Request>,
    /// The listener and its thread, stopped and its socket removed when the
    /// session ends.
    _socket: Socket,
}

/// One ask an app made over its own authenticated connection.
struct Request {
    /// The app the launcher vouched for.
    app: String,
    /// The room its launch runs it in.
    room: String,
    message: FloatingSurfaceClientMessage,
}

impl AppTabs {
    /// Serve the shell-ui socket, waking the session's loop with each ask.
    ///
    /// # Errors
    /// The service's directory cannot be made, or its socket cannot be bound
    /// or served.
    pub fn start(launches: Arc<Launches>, wake: tilo_service::Wake) -> Result<Self, String> {
        let root = launcher::services::ensure_services_root()
            .map_err(|error| format!("create the shell-ui service directory: {error}"))?;
        let path = root.join(format!("{SHELL_UI_SERVICE}.sock"));
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove the stale shell-ui socket: {error}"))?;
        }
        let listener = UnixListener::bind(&path)
            .map_err(|error| format!("bind the shell-ui socket {}: {error}", path.display()))?;
        let (requests, answers) = mpsc::channel();
        let socket = Socket::serve(listener, path, launches, requests, wake)?;
        Ok(Self {
            requests: answers,
            _socket: socket,
        })
    }

    /// Answer what the apps asked since the last pass: each ask is a desk
    /// command, so the desk is read and changed on this thread alone.
    pub fn follow(&self, desktop: &DesktopHost, apps: &Apps) {
        for request in self.requests.try_iter() {
            answer(desktop, apps, request);
        }
    }
}

/// Answer one ask of the app it came from, or nothing when it cannot be
/// answered: an ask never changes another app's tab.
fn answer(desktop: &DesktopHost, apps: &Apps, request: Request) {
    let Request { app, room, message } = request;
    let anchor = desktop.with_policy(|policy| policy.anchor_for(&app).map(|(tab, ..)| tab));
    match message {
        FloatingSurfaceClientMessage::OpenTabBeside {
            target, background, ..
        } => {
            let open = if let Some(target) = target {
                let Some(open) = target_open(&app, &target) else {
                    tracing::info!(%app, "an app asked to open an address it does not claim");
                    return;
                };
                Some(open)
            } else {
                None
            };
            let launch = Launch::Package {
                app: app.clone(),
                open,
            };
            // The place it opens into is there from this frame; a launch
            // that starts nothing takes it back.
            let held = desktop.with_policy(|policy| policy.hold_beside(&app, anchor, background));
            if let Err(error) = apps.start_beside(&launch, &apps.room_named(&room)) {
                if held {
                    desktop.with_policy(|policy| policy.release_held(&app));
                }
                tracing::warn!(%error, %app, "a window the app asked for did not start");
            }
        }
        FloatingSurfaceClientMessage::CloseTab { .. } => {
            desktop.with_policy(|policy| policy.close_tab_of(&app));
        }
        FloatingSurfaceClientMessage::ReopenClosedTab => {
            desktop.with_policy(DesktopPolicy::reopen_closed);
        }
        FloatingSurfaceClientMessage::CycleTab { forward, .. } => {
            desktop.with_policy(|policy| policy.cycle_tab(&app, forward));
        }
        FloatingSurfaceClientMessage::PlaceNextWindowBeside { background, .. } => {
            desktop.with_policy(|policy| policy.place_next_beside(&app, anchor, background));
        }
        _ => {}
    }
}

/// The address `app` asks to open, when its manifest claims it: a link it
/// declares the scheme of, as its own menu would open one.
fn target_open(app: &str, target: &str) -> Option<Open> {
    if target.is_empty()
        || target.len() > MAX_TARGET_BYTES
        || target.trim() != target
        || target.chars().any(char::is_control)
    {
        return None;
    }
    let scheme = routing::scheme_of(target)?;
    let installed = tilo_installed_apps::read_one_from_env(app).ok().flatten()?;
    installed
        .opens_uri_schemes
        .iter()
        .any(|claims| claims.eq_ignore_ascii_case(&scheme))
        .then(|| Open::Uri(target.to_owned()))
}

/// The listener serving the socket, and the thread taking connections off it.
struct Socket {
    stop: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
    path: PathBuf,
}

impl Socket {
    fn serve(
        listener: UnixListener,
        path: PathBuf,
        launches: Arc<Launches>,
        requests: mpsc::Sender<Request>,
        wake: tilo_service::Wake,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let acceptor = std::thread::Builder::new()
            .name("tilo-shell-ui-accept".into())
            .spawn(move || {
                let next = AtomicU64::new(1);
                loop {
                    match listener.accept() {
                        Ok(_) if thread_stop.load(Ordering::Acquire) => break,
                        Ok((stream, _)) => {
                            let connection = next.fetch_add(1, Ordering::Relaxed);
                            let (launches, requests, wake) =
                                (Arc::clone(&launches), requests.clone(), wake.clone());
                            let stop = Arc::clone(&thread_stop);
                            let _ = std::thread::Builder::new()
                                .name(format!("tilo-shell-ui-{connection}"))
                                .spawn(move || {
                                    serve_connection(
                                        connection, stream, &launches, &requests, &wake, &stop,
                                    );
                                });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            if thread_stop.load(Ordering::Acquire) {
                                break;
                            }
                            tracing::warn!(%error, "the shell-ui service could not accept");
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            })
            .map_err(|error| {
                let _ = std::fs::remove_file(&path);
                format!("spawn the shell-ui service: {error}")
            })?;
        Ok(Self {
            stop,
            acceptor: Some(acceptor),
            path,
        })
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // The acceptor waits in `accept`; one connection wakes it to see the
        // flag. If nothing can connect it is not waited for.
        if let Some(acceptor) = self.acceptor.take()
            && UnixStream::connect(&self.path).is_ok()
        {
            let _ = acceptor.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The launcher's word for a connection, written before the sandbox starts:
/// which app it is, as the launcher knows it.
#[derive(Deserialize)]
struct LauncherHandshake {
    protocol: u16,
    app_id: String,
}

/// The app's first frame as every shell-ui version reads it:
/// `{"type": "hello", "version": N}`, whatever else a later version adds.
#[derive(Deserialize)]
struct Hello {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    version: Option<u16>,
}

/// The shell-ui version `frame` says, when it is a hello.
fn hello_version(frame: &[u8]) -> Option<u16> {
    serde_json::from_slice::<Hello>(frame)
        .ok()
        .filter(|hello| hello.kind == "hello")
        .and_then(|hello| hello.version)
}

// The channel handles are deliberately owned by the per-connection worker,
// which outlives the accept loop's stack frame.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the per-connection worker owns its handles and outlives the accept loop"
)]
fn serve_connection(
    connection: u64,
    stream: UnixStream,
    launches: &Launches,
    requests: &mpsc::Sender<Request>,
    wake: &tilo_service::Wake,
    stop: &AtomicBool,
) {
    // The process at the other end, as the socket says it: a window the app
    // opened, or the session's own connection for one it opened in the app.
    let peer = peer_pid(&stream);
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let Some(handshake_frame) = wire_frame::read_line(&mut reader, MAX_FRAME_BYTES)
        .ok()
        .flatten()
    else {
        return;
    };
    let Ok(handshake) = serde_json::from_slice::<LauncherHandshake>(&handshake_frame) else {
        return;
    };
    if handshake.protocol != SHELL_UI_LAUNCH_PROTOCOL
        || handshake.app_id.trim().is_empty()
        || handshake.app_id.len() > 255
    {
        return;
    }
    // The app's first frame says the version it speaks; nothing else is read
    // from an app at another one.
    let Ok(Some(hello)) = wire_frame::read_line(&mut reader, MAX_FRAME_BYTES) else {
        return;
    };
    let app_version = hello_version(&hello);
    if app_version != Some(FLOATING_SURFACE_PROTOCOL_VERSION) {
        refuse_version(&mut writer, &handshake.app_id, app_version);
        return;
    }
    let app = handshake.app_id;
    let Some(room) = launched_room(launches, peer, &app) else {
        tracing::info!(%app, "a shell-ui connection names no app this session started");
        return;
    };
    while !stop.load(Ordering::Acquire) {
        let frame = match wire_frame::read_line(&mut reader, MAX_FRAME_BYTES) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, %app, "a shell-ui frame was refused");
                break;
            }
        };
        let Ok(message) = serde_json::from_slice::<FloatingSurfaceClientMessage>(&frame) else {
            tracing::warn!(%app, "a shell-ui message was refused");
            break;
        };
        if !asks_about_tabs(&message) {
            // Vision, markers and the window's own lifecycle are not this
            // service's; the rest of the connection keeps being read.
            continue;
        }
        if requests
            .send(Request {
                app: app.clone(),
                room: room.clone(),
                message,
            })
            .is_err()
        {
            break;
        }
        wake();
    }
    tracing::debug!(connection, %app, "shell-ui connection closed");
}

/// The process at the other end of `stream`, as the kernel names it: the
/// app's own process, or the session itself for a window it connected.
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).ok()?;
    // SAFETY: both pointers name initialized storage of `length` bytes and
    // the socket stays open for the duration of `getsockopt`.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut credentials).cast(),
            std::ptr::from_mut(&mut length),
        )
    };
    if result != 0 || usize::try_from(length).ok()? < std::mem::size_of::<libc::ucred>() {
        return None;
    }
    u32::try_from(credentials.pid).ok().filter(|pid| *pid > 0)
}

/// The room the app the connection names runs in, when this session started
/// it: the launch the peer's process belongs to.
fn launched_room(launches: &Launches, peer: Option<u32>, app: &str) -> Option<String> {
    let pid = match peer {
        // The session connects a window it opens in an app's running process
        // itself: what it recorded for that connection names the launch.
        Some(pid) if pid == std::process::id() => launches.take_window_shell_ui(app)?,
        peer => peer?,
    };
    launches.room_of(pid, app)
}

/// Whether `message` is one this service answers.
fn asks_about_tabs(message: &FloatingSurfaceClientMessage) -> bool {
    matches!(
        message,
        FloatingSurfaceClientMessage::OpenTabBeside { .. }
            | FloatingSurfaceClientMessage::CloseTab { .. }
            | FloatingSurfaceClientMessage::ReopenClosedTab
            | FloatingSurfaceClientMessage::CycleTab { .. }
            | FloatingSurfaceClientMessage::PlaceNextWindowBeside { .. }
    )
}

/// Tell an app speaking another shell-ui version which one this session
/// speaks, so the one behind can be updated.
fn refuse_version(writer: &mut UnixStream, app: &str, app_version: Option<u16>) {
    if let Ok(mut answer) = serde_json::to_vec(&FloatingSurfaceHostMessage::VersionMismatch {
        shell: FLOATING_SURFACE_PROTOCOL_VERSION,
    }) {
        answer.push(b'\n');
        let _ = writer.write_all(&answer).and_then(|()| writer.flush());
    }
    tracing::info!(
        %app,
        ?app_version,
        shell = FLOATING_SURFACE_PROTOCOL_VERSION,
        "shell-ui version mismatch"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the launcher writes before an app's sandbox starts.
    fn handshake(app: &str) -> Vec<u8> {
        let mut frame = serde_json::to_vec(&serde_json::json!({
            "protocol": SHELL_UI_LAUNCH_PROTOCOL,
            "app_id": app,
            "network_status_allowed": false,
        }))
        .unwrap();
        frame.push(b'\n');
        frame
    }

    #[test]
    fn only_tab_asks_are_answered_here() {
        assert!(asks_about_tabs(
            &FloatingSurfaceClientMessage::ReopenClosedTab
        ));
        assert!(asks_about_tabs(&FloatingSurfaceClientMessage::CloseTab {
            instance: None
        }));
        assert!(!asks_about_tabs(&FloatingSurfaceClientMessage::Hello {
            version: 1
        }));
        assert!(!asks_about_tabs(
            &FloatingSurfaceClientMessage::PaintCapable
        ));
    }

    #[test]
    fn the_launcher_s_handshake_and_the_app_s_hello_are_read() {
        let frame = handshake("so.tilo.notes");
        let parsed: LauncherHandshake = serde_json::from_slice(&frame[..frame.len() - 1]).unwrap();
        assert_eq!(parsed.protocol, SHELL_UI_LAUNCH_PROTOCOL);
        assert_eq!(parsed.app_id, "so.tilo.notes");
        let mut hello = serde_json::to_vec(&FloatingSurfaceClientMessage::Hello {
            version: FLOATING_SURFACE_PROTOCOL_VERSION,
        })
        .unwrap();
        hello.push(b'\n');
        assert_eq!(
            hello_version(&hello[..hello.len() - 1]),
            Some(FLOATING_SURFACE_PROTOCOL_VERSION)
        );
        assert_eq!(
            hello_version(b"{\"type\":\"hello\",\"version\":0}"),
            Some(0)
        );
        assert_eq!(hello_version(b"{\"type\":\"other\"}"), None);
    }
}
