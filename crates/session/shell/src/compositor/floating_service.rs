//! Authenticated app ↔ compositor lifecycle channel for floating surfaces.
//!
//! The launcher opens the socket and writes the canonical app id before the
//! sandbox starts. Only after that handshake is accepted do client lifecycle
//! messages enter the compositor. This prevents a sandboxed app from naming a
//! different app id and turning `PiP` into a cross-app capture primitive.
//! The app's own first frame says the shell-ui version it speaks; an app at
//! another version is answered with the shell's and the connection closes
//! (docs/architecture/compatibility.md).

use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use launcher::services::SHELL_UI_LAUNCH_PROTOCOL;
use ui::floating_surface::{
    FLOATING_SURFACE_PROTOCOL_VERSION, FLOATING_SURFACE_SERVICE, FloatingSurfaceClientMessage,
    FloatingSurfaceHostMessage, HostNetworkStatus,
};

const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "events are moved once through a channel; boxing the message would add an allocation per frame"
)]
pub enum FloatingServiceEvent {
    Connected {
        connection: u64,
        app_id: String,
        network_status_allowed: bool,
    },
    Message {
        connection: u64,
        app_id: String,
        launch: Option<AuthenticatedLaunch>,
        message: FloatingSurfaceClientMessage,
    },
    Disconnected {
        connection: u64,
        app_id: String,
    },
    /// The app said another shell-ui version than this shell's (`None`: it
    /// said none, so it predates the version check). The shell answered
    /// with its own and closed the connection; the app is updated or the
    /// system is, whichever is behind (docs/architecture/compatibility.md).
    VersionMismatch {
        app_id: String,
        app_version: Option<u16>,
    },
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

/// Opaque authenticated launch custody for event observation.
/// External callers cannot construct it or access its custody fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatedLaunch {
    pub(crate) workspace_id: String,
    pub(crate) custody: crate::shell::launcher::VerifiedLaunchCustody,
}

#[derive(Deserialize)]
struct LauncherHandshake {
    protocol: u16,
    app_id: String,
    #[serde(default)]
    network_status_allowed: bool,
}

/// The service owns shutdown even when a reply sender is held by a worker.
struct ConnectionWriter {
    sender: mpsc::Sender<Vec<u8>>,
    socket: UnixStream,
}

impl Drop for ConnectionWriter {
    fn drop(&mut self) {
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        // An empty buffer is an internal shutdown marker, never a wire frame.
        let _ = self.sender.send(Vec::new());
    }
}

pub struct FloatingSurfaceService {
    events: mpsc::Receiver<FloatingServiceEvent>,
    writers: Arc<Mutex<HashMap<u64, ConnectionWriter>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    acceptor: Option<std::thread::JoinHandle<()>>,
    path: PathBuf,
}

impl FloatingSurfaceService {
    /// Start the authenticated floating-surface lifecycle service.
    ///
    /// # Errors
    ///
    /// Returns an error if its runtime directory, socket, or accept thread
    /// cannot be created.
    pub fn start() -> Result<Self, String> {
        let root = launcher::services::ensure_services_root()
            .map_err(|error| format!("create shell-ui service directory: {error}"))?;
        let path = root.join(format!("{FLOATING_SURFACE_SERVICE}.sock"));
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove stale shell-ui socket: {error}"))?;
        }
        let listener = UnixListener::bind(&path)
            .map_err(|error| format!("bind shell-ui service {}: {error}", path.display()))?;

        let (event_tx, event_rx) = mpsc::channel();
        let writers = Arc::new(Mutex::new(HashMap::new()));
        let thread_writers = Arc::clone(&writers);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let acceptor = std::thread::Builder::new()
            .name("tilo-shell-ui-accept".into())
            .spawn(move || {
                let next_id = AtomicU64::new(1);
                loop {
                    match listener.accept() {
                        Ok(_) if thread_stop.load(std::sync::atomic::Ordering::Acquire) => break,
                        Ok((stream, _)) => {
                            let connection = next_id.fetch_add(1, Ordering::Relaxed);
                            let tx = event_tx.clone();
                            let writers = Arc::clone(&thread_writers);
                            let stop = Arc::clone(&thread_stop);
                            let _ = std::thread::Builder::new()
                                .name(format!("tilo-shell-ui-{connection}"))
                                .spawn(move || {
                                    serve_connection(connection, stream, tx, writers, stop);
                                });
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            if thread_stop.load(std::sync::atomic::Ordering::Acquire) {
                                break;
                            }
                            tracing::warn!(%error, "shell-ui service accept failed");
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            })
            .map_err(|error| {
                let _ = std::fs::remove_file(&path);
                format!("spawn shell-ui service: {error}")
            })?;

        Ok(Self {
            events: event_rx,
            writers,
            stop,
            acceptor: Some(acceptor),
            path,
        })
    }

    pub fn drain(&self) -> Vec<FloatingServiceEvent> {
        self.events.try_iter().collect()
    }

    pub fn reply_writer(&self, connection: u64) -> Option<mpsc::Sender<Vec<u8>>> {
        self.writers
            .lock()
            .ok()?
            .get(&connection)
            .map(|writer| writer.sender.clone())
    }

    pub fn send(&self, connection: u64, message: &FloatingSurfaceHostMessage) {
        let Ok(mut frame) = serde_json::to_vec(message) else {
            return;
        };
        frame.push(b'\n');
        let writer = self
            .writers
            .lock()
            .ok()
            .and_then(|writers| writers.get(&connection).map(|writer| writer.sender.clone()));
        let failed = writer.is_some_and(|writer| writer.send(frame).is_err());
        if failed && let Ok(mut writers) = self.writers.lock() {
            writers.remove(&connection);
        }
    }

    /// Publish compositor-owned environment changes (currently display
    /// bounds) to every authenticated app. A dock, rotation or mode switch
    /// must update an already-running drag just as surely as a newly opened
    /// connection; otherwise the app and shell clamp against different
    /// desktops until the next ownership transfer.
    pub fn broadcast(&self, message: &FloatingSurfaceHostMessage) {
        let Ok(mut frame) = serde_json::to_vec(message) else {
            return;
        };
        frame.push(b'\n');
        let writers = self
            .writers
            .lock()
            .map(|writers| {
                writers
                    .iter()
                    .map(|(connection, writer)| (*connection, writer.sender.clone()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let failed = writers
            .into_iter()
            .filter_map(|(connection, writer)| {
                writer.send(frame.clone()).is_err().then_some(connection)
            })
            .collect::<Vec<_>>();
        if !failed.is_empty()
            && let Ok(mut writers) = self.writers.lock()
        {
            for connection in failed {
                writers.remove(&connection);
            }
        }
    }
}

impl Drop for FloatingSurfaceService {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        // The acceptor sleeps in `accept`; one connection wakes it to see the
        // flag. If nothing can connect it is not waited for.
        if let Some(acceptor) = self.acceptor.take()
            && UnixStream::connect(&self.path).is_ok()
        {
            let _ = acceptor.join();
        }
        if let Ok(mut writers) = self.writers.lock() {
            writers.clear();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

// These channel handles are deliberately owned by the per-connection worker,
// which can outlive the accept-loop stack frame.
#[allow(
    clippy::needless_pass_by_value,
    reason = "the per-connection worker owns its channel handles and may outlive the accept loop"
)]
fn serve_connection(
    connection: u64,
    stream: UnixStream,
    events: mpsc::Sender<FloatingServiceEvent>,
    writers: Arc<Mutex<HashMap<u64, ConnectionWriter>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    let peer_pid = super::native::wayland_peer_credentials(&stream)
        .and_then(|credentials| u32::try_from(credentials.pid).ok());
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
        if let Ok(mut answer) = serde_json::to_vec(&FloatingSurfaceHostMessage::VersionMismatch {
            shell: FLOATING_SURFACE_PROTOCOL_VERSION,
        }) {
            answer.push(b'\n');
            let _ = writer.write_all(&answer).and_then(|()| writer.flush());
        }
        tracing::info!(
            app_id = %handshake.app_id,
            ?app_version,
            shell = FLOATING_SURFACE_PROTOCOL_VERSION,
            "shell-ui version mismatch"
        );
        let _ = events.send(FloatingServiceEvent::VersionMismatch {
            app_id: handshake.app_id,
            app_version,
        });
        return;
    }
    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>();
    if let Ok(mut open) = writers.lock() {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let Ok(socket) = reader.get_ref().try_clone() else {
            return;
        };
        open.insert(
            connection,
            ConnectionWriter {
                sender: write_tx,
                socket,
            },
        );
    } else {
        return;
    }
    let _ = std::thread::Builder::new()
        .name(format!("tilo-shell-ui-writer-{connection}"))
        .spawn(move || {
            for frame in write_rx {
                if frame.is_empty() {
                    break;
                }
                if writer
                    .write_all(&frame)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
    let app_id = handshake.app_id;
    let launch = authenticated_launch(peer_pid, &app_id);
    let _ = events.send(FloatingServiceEvent::Connected {
        network_status_allowed: handshake.network_status_allowed,
        connection,
        app_id: app_id.clone(),
    });

    loop {
        let frame = match wire_frame::read_line(&mut reader, MAX_FRAME_BYTES) {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(%error, %app_id, "shell-ui client frame rejected");
                break;
            }
        };
        let message = match serde_json::from_slice::<FloatingSurfaceClientMessage>(&frame) {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, %app_id, "shell-ui client message rejected");
                break;
            }
        };
        if events
            .send(FloatingServiceEvent::Message {
                connection,
                app_id: app_id.clone(),
                launch: launch.clone(),
                message,
            })
            .is_err()
        {
            break;
        }
    }
    if let Ok(mut open) = writers.lock() {
        open.remove(&connection);
    }
    let _ = events.send(FloatingServiceEvent::Disconnected { connection, app_id });
}

fn authenticated_launch(peer_pid: Option<u32>, app_id: &str) -> Option<AuthenticatedLaunch> {
    peer_pid
        .and_then(crate::shell::launcher::verified_launch_custody_for_descendant)
        .and_then(|(identity, custody)| {
            (identity.confined && identity.package_id == app_id)
                .then_some(identity.workspace_id)
                .flatten()
                .map(|workspace_id| AuthenticatedLaunch {
                    workspace_id,
                    custody,
                })
        })
}

/// What each authenticated app was last told about where, and for whom,
/// its window is shown ([`ui::WindowAppearance`]), so it is told again only
/// when that changes.
#[derive(Debug, Default)]
pub struct AppearanceDelivery {
    told: HashMap<u64, (String, bool, Option<DeliveredAppearance>)>,
}

#[derive(Debug)]
struct DeliveredAppearance {
    window: ui::WindowAppearance,
    theme: ui::theme::ThemeDocument,
    network: Option<HostNetworkStatus>,
}

impl AppearanceDelivery {
    /// `app_id` connected on `connection`; it has been told nothing yet.
    pub fn connected(&mut self, connection: u64, app_id: &str, network_allowed: bool) {
        self.told
            .insert(connection, (app_id.to_owned(), network_allowed, None));
    }

    pub fn disconnected(&mut self, connection: u64) {
        self.told.remove(&connection);
    }

    /// Tell every connection whose window's appearance (`appearance_of` its
    /// app id) differs from what it was last told.
    pub fn publish(
        &mut self,
        appearance_of: impl Fn(&str) -> ui::WindowAppearance,
        theme: &ui::Theme,
        network: HostNetworkStatus,
        mut send: impl FnMut(u64, &FloatingSurfaceHostMessage),
    ) {
        for (connection, (app_id, network_allowed, told)) in &mut self.told {
            let network = network_allowed.then_some(network);
            let appearance = appearance_of(app_id);
            if told.as_ref().is_none_or(|last| {
                last.window != appearance || last.theme != theme.document || last.network != network
            }) {
                let mut message =
                    FloatingSurfaceHostMessage::appearance_with_theme(&appearance, theme);
                if let FloatingSurfaceHostMessage::Appearance { network_status, .. } = &mut message
                {
                    *network_status = network;
                }
                send(*connection, &message);
                *told = Some(DeliveredAppearance {
                    window: appearance,
                    theme: theme.document.clone(),
                    network,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::io::BufRead as _;
    use std::os::fd::BorrowedFd;
    use std::time::Instant;

    /// What an app writes first: the shell-ui version it speaks.
    fn say_hello(app: &mut UnixStream, version: u16) {
        let mut frame =
            serde_json::to_vec(&FloatingSurfaceClientMessage::Hello { version }).unwrap();
        frame.push(b'\n');
        app.write_all(&frame).unwrap();
    }

    /// The descriptor the launcher hands the app, cloned out of `resolved`.
    fn child_of(resolved: &[launcher::services::ResolvedService]) -> UnixStream {
        // SAFETY: resolved owns this host descriptor throughout; the borrow
        // only clones it into a separately owned descriptor.
        let borrowed = unsafe { BorrowedFd::borrow_raw(resolved[0].host_fd()) };
        UnixStream::from(borrowed.try_clone_to_owned().expect("child descriptor"))
    }

    #[test]
    fn each_app_is_told_its_window_appearance_once_per_change() {
        use std::cell::Cell;
        use ui::tokens::PresenceHue;

        let warm = ui::WindowAppearance {
            workspace_hue: Some(40.0),
            your_colour: Some(PresenceHue::Teal),
        };
        let grey = ui::WindowAppearance {
            workspace_hue: None,
            your_colour: Some(PresenceHue::Teal),
        };
        let rooms = Cell::new((warm, grey));
        let appearance_of = |app: &str| {
            let (notes, files) = rooms.get();
            if app == "so.tilo.notes" { notes } else { files }
        };
        let mut delivery = AppearanceDelivery::default();
        delivery.connected(1, "so.tilo.notes", false);
        delivery.connected(2, "so.tilo.files", false);
        let network = HostNetworkStatus::default();
        let mut sent = Vec::new();
        delivery.publish(
            appearance_of,
            &ui::Theme::dark(),
            network,
            |connection, message| {
                sent.push((connection, message.window_appearance()));
            },
        );
        sent.sort_by_key(|(connection, _)| *connection);
        assert_eq!(sent, vec![(1, Some(warm)), (2, Some(grey))]);

        let mut palettes = Vec::new();
        delivery.publish(
            appearance_of,
            &ui::Theme::light(),
            network,
            |connection, message| {
                palettes.push((connection, message.configured_theme().unwrap().document));
            },
        );
        palettes.sort_by_key(|(connection, _)| *connection);
        assert_eq!(
            palettes,
            vec![
                (1, ui::Theme::light().document),
                (2, ui::Theme::light().document)
            ]
        );
        palettes.clear();
        delivery.publish(
            appearance_of,
            &ui::Theme::light(),
            network,
            |connection, message| {
                palettes.push((connection, message.configured_theme().unwrap().document));
            },
        );
        assert!(
            palettes.is_empty(),
            "an unchanged palette is not sent every frame"
        );
        delivery.publish(appearance_of, &ui::Theme::dark(), network, |_, _| {});

        sent.clear();
        delivery.publish(
            appearance_of,
            &ui::Theme::dark(),
            network,
            |connection, message| {
                sent.push((connection, message.window_appearance()));
            },
        );
        assert!(sent.is_empty(), "nothing changed, nothing is said");

        rooms.set((grey, grey));
        delivery.publish(
            appearance_of,
            &ui::Theme::dark(),
            network,
            |connection, message| {
                sent.push((connection, message.window_appearance()));
            },
        );
        assert_eq!(sent, vec![(1, Some(grey))], "only the window that moved");

        sent.clear();
        delivery.disconnected(1);
        rooms.set((warm, warm));
        delivery.publish(
            appearance_of,
            &ui::Theme::dark(),
            network,
            |connection, message| {
                sent.push((connection, message.window_appearance()));
            },
        );
        assert_eq!(
            sent,
            vec![(2, Some(warm))],
            "a closed connection is forgotten"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn the_launcher_s_handshake_is_accepted() {
        let runtime = tempfile::tempdir().expect("temporary runtime");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", runtime.path());
        let service = FloatingSurfaceService::start().expect("start floating service");
        let mut app = UnixStream::connect(launcher::services::socket_for(FLOATING_SURFACE_SERVICE))
            .expect("connect");
        // The exact frame `launcher::services::authenticate_shell_ui_bind`
        // writes before the app starts.
        let mut handshake = serde_json::to_vec(&serde_json::json!({
            "protocol": SHELL_UI_LAUNCH_PROTOCOL,
            "app_id": "so.tilo.notes",
        }))
        .unwrap();
        handshake.push(b'\n');
        app.write_all(&handshake).unwrap();
        say_hello(&mut app, FLOATING_SURFACE_PROTOCOL_VERSION);
        let mut connected = None;
        tilo_test_env::wait_until("the authenticated app connects", || {
            connected = service.drain().into_iter().find_map(|event| match event {
                FloatingServiceEvent::Connected { app_id, .. } => Some(app_id),
                _ => None,
            });
            connected.is_some()
        });
        assert_eq!(connected.as_deref(), Some("so.tilo.notes"));
        drop(service);
    }

    #[test]
    fn network_delivery_preserves_unknown_and_permissions_without_personal_metadata() {
        let legacy: LauncherHandshake =
            serde_json::from_str(r#"{"protocol":1,"app_id":"so.tilo.notes"}"#).unwrap();
        assert!(!legacy.network_status_allowed);
        let mut delivery = AppearanceDelivery::default();
        delivery.connected(1, "so.tilo.notes", true);
        delivery.connected(2, "so.tilo.files", false);
        let theme = ui::Theme::dark();
        let mut sent = Vec::new();
        delivery.publish(
            |_| ui::WindowAppearance::default(),
            &theme,
            HostNetworkStatus::default(),
            |id, message| sent.push((id, serde_json::to_value(message).unwrap())),
        );
        let allowed = &sent.iter().find(|(id, _)| *id == 1).unwrap().1;
        assert_eq!(
            allowed["network_status"],
            serde_json::json!({"available":null,"metered":null})
        );
        assert!(
            sent.iter()
                .find(|(id, _)| *id == 2)
                .unwrap()
                .1
                .get("network_status")
                .is_none()
        );
        for metered in [Some(true), Some(false), None] {
            sent.clear();
            let network = HostNetworkStatus {
                available: Some(true),
                metered,
            };
            delivery.publish(
                |_| ui::WindowAppearance::default(),
                &theme,
                network,
                |id, message| sent.push((id, serde_json::to_value(message).unwrap())),
            );
            assert_eq!(
                sent.len(),
                1,
                "a network-denied app receives no status transition"
            );
            assert_eq!(sent[0].0, 1);
            assert_eq!(
                sent[0].1["network_status"],
                serde_json::json!({"available":true,"metered":metered})
            );
            sent.clear();
            delivery.publish(
                |_| ui::WindowAppearance::default(),
                &theme,
                network,
                |id, message| sent.push((id, serde_json::to_value(message).unwrap())),
            );
            assert!(
                sent.is_empty(),
                "idle/deduplicated status does not send another frame"
            );
        }
        delivery.disconnected(1);
        sent.clear();
        delivery.publish(
            |_| ui::WindowAppearance::default(),
            &theme,
            HostNetworkStatus {
                available: Some(false),
                metered: None,
            },
            |id, message| sent.push((id, serde_json::to_value(message).unwrap())),
        );
        assert!(
            sent.is_empty(),
            "disconnected/denied clients receive nothing"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn authenticated_terminal_receives_owner_selection_on_its_existing_lifecycle_connection() {
        let runtime = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", runtime.path());
        let service = FloatingSurfaceService::start().unwrap();
        let declared = BTreeMap::from([(
            launcher::services::SHELL_UI_SERVICE.to_owned(),
            launcher::manifest::ServiceReq::parse(
                launcher::services::SHELL_UI_SERVICE,
                "required@1",
            )
            .unwrap(),
        )]);
        let resolved = launcher::services::resolve_service_binds(&declared, false).unwrap();
        launcher::services::authenticate_shell_ui_bind(
            &resolved,
            crate::shell::host_shell_allow::TERMINAL_APP_ID,
            &launcher::netgate::EgressDecision::Blocked,
        )
        .unwrap();
        let mut child = child_of(&resolved);
        say_hello(&mut child, FLOATING_SURFACE_PROTOCOL_VERSION);
        let FloatingServiceEvent::Connected {
            connection, app_id, ..
        } = service.events.recv_timeout(Duration::from_secs(2)).unwrap()
        else {
            panic!("expected authenticated Terminal connection");
        };
        assert_eq!(app_id, crate::shell::host_shell_allow::TERMINAL_APP_ID);
        child
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        service.send(
            connection,
            &FloatingSurfaceHostMessage::DeviceShellOperation {
                request: 1,
                operation: ui::floating_surface::DeviceShellOperation::Select,
            },
        );
        let mut reader = BufReader::new(child);
        let frame = wire_frame::read_line(&mut reader, MAX_FRAME_BYTES)
            .unwrap()
            .expect("actual host wire frame");
        assert_eq!(
            serde_json::from_slice::<FloatingSurfaceHostMessage>(&frame).unwrap(),
            FloatingSurfaceHostMessage::DeviceShellOperation {
                request: 1,
                operation: ui::floating_surface::DeviceShellOperation::Select
            }
        );
        drop(reader);
        drop(resolved);
        assert!(
            matches!(service.events.recv_timeout(Duration::from_secs(2)), Ok(FloatingServiceEvent::Disconnected { connection: ended, .. }) if ended == connection)
        );
        assert!(service.reply_writer(connection).is_none());
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn launcher_authentication_accepts_current_floating_payload() {
        let runtime = tempfile::tempdir().expect("temporary runtime");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", runtime.path());
        let service = FloatingSurfaceService::start().expect("start service");
        let declared = BTreeMap::from([(
            launcher::services::SHELL_UI_SERVICE.to_owned(),
            launcher::manifest::ServiceReq::parse(
                launcher::services::SHELL_UI_SERVICE,
                "required@1",
            )
            .unwrap(),
        )]);
        let resolved = launcher::services::resolve_service_binds(&declared, false)
            .expect("resolve the real launch descriptor");
        launcher::services::authenticate_shell_ui_bind(
            &resolved,
            "so.tilo.chat",
            &launcher::netgate::EgressDecision::Unrestricted,
        )
        .expect("write the production launcher handshake");
        let mut child = child_of(&resolved);
        say_hello(&mut child, FLOATING_SURFACE_PROTOCOL_VERSION);
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)),
            Ok(FloatingServiceEvent::Connected { app_id, network_status_allowed: true, .. }) if app_id == "so.tilo.chat"
        ));
        let request = ui::floating_surface::FloatingSurfaceProtocolRequest {
            id: "conversation".into(),
            title: "Agent conversation".into(),
            role: ui::floating_surface::FloatingSurfaceProtocolRole::LiveActivity,
            source: ui::floating_surface::FloatingSurfaceProtocolSource::AppElement {
                app_id: "so.tilo.chat".into(),
                element_id: "conversation".into(),
                region: [0.0, 0.0, 560.0, 560.0],
            },
            preferred_size: [560.0, 560.0],
            aspect_ratio: None,
            initial_rect: None,
            presentation_rect: None,
        };
        let message = FloatingSurfaceClientMessage::Present { request };
        let mut frame = serde_json::to_vec(&message).expect("serialize current lifecycle");
        frame.push(b'\n');
        child.write_all(&frame).expect("send current lifecycle");
        match service.events.recv_timeout(Duration::from_secs(2)) {
            Ok(FloatingServiceEvent::Message {
                app_id,
                message: received,
                ..
            }) => {
                assert_eq!(app_id, "so.tilo.chat");
                assert_eq!(received, message);
            }
            other => panic!("authenticated lifecycle was not delivered: {other:?}"),
        }
        drop(child);
        drop(resolved);
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)),
            Ok(FloatingServiceEvent::Disconnected { app_id, .. }) if app_id == "so.tilo.chat"
        ));
    }

    /// An app at another shell-ui version is told the shell's and closed
    /// before anything it says reaches the compositor; the shell hears which
    /// version it spoke, to update the side that is behind.
    #[test]
    fn an_app_at_another_version_is_answered_with_the_shells_and_closed() {
        let older = FLOATING_SURFACE_PROTOCOL_VERSION - 1;
        let first_frames: [(Vec<u8>, Option<u16>); 3] = [
            (
                serde_json::to_vec(&FloatingSurfaceClientMessage::Hello { version: older })
                    .unwrap(),
                Some(older),
            ),
            (
                serde_json::to_vec(&serde_json::json!({
                    "type": "hello",
                    "version": FLOATING_SURFACE_PROTOCOL_VERSION + 1,
                    "something_later": true,
                }))
                .unwrap(),
                Some(FLOATING_SURFACE_PROTOCOL_VERSION + 1),
            ),
            (
                serde_json::to_vec(&FloatingSurfaceClientMessage::Dismiss { id: "call".into() })
                    .unwrap(),
                None,
            ),
        ];
        for (first, said) in first_frames {
            let (mut app, shell) = UnixStream::pair().expect("connected socket pair");
            let (tx, rx) = mpsc::channel();
            let writers = Arc::new(Mutex::new(HashMap::new()));
            let observed = Arc::clone(&writers);
            let worker = std::thread::spawn(move || {
                serve_connection(
                    1,
                    shell,
                    tx,
                    writers,
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                );
            });
            let mut handshake = serde_json::to_vec(&serde_json::json!({
                "protocol": SHELL_UI_LAUNCH_PROTOCOL,
                "app_id": "so.tilo.mail",
            }))
            .unwrap();
            handshake.push(b'\n');
            app.write_all(&handshake).unwrap();
            let mut frame = first;
            frame.push(b'\n');
            app.write_all(&frame).unwrap();
            worker.join().expect("service worker exits");

            let mut answer = String::new();
            BufReader::new(app).read_line(&mut answer).unwrap();
            assert_eq!(
                serde_json::from_str::<FloatingSurfaceHostMessage>(answer.trim()).unwrap(),
                FloatingSurfaceHostMessage::VersionMismatch {
                    shell: FLOATING_SURFACE_PROTOCOL_VERSION
                }
            );
            let events: Vec<_> = rx.try_iter().collect();
            assert_eq!(events.len(), 1, "{events:?}");
            assert!(
                matches!(
                    &events[0],
                    FloatingServiceEvent::VersionMismatch { app_id, app_version }
                        if app_id == "so.tilo.mail" && *app_version == said
                ),
                "{events:?}"
            );
            assert!(observed.lock().expect("writers").is_empty());
        }
    }

    #[test]
    fn launch_authentication_refuses_payload_version_and_invalid_identity() {
        for (protocol, app_id) in [
            (
                ui::floating_surface::FLOATING_SURFACE_PROTOCOL_VERSION,
                "so.tilo.chat".to_owned(),
            ),
            (0, "so.tilo.chat".to_owned()),
            (SHELL_UI_LAUNCH_PROTOCOL, " ".to_owned()),
            (SHELL_UI_LAUNCH_PROTOCOL, "x".repeat(256)),
        ] {
            let (mut client, server) = UnixStream::pair().expect("connected socket pair");
            let (tx, rx) = mpsc::channel();
            let writers = Arc::new(Mutex::new(HashMap::new()));
            let observed = Arc::clone(&writers);
            let worker = std::thread::spawn(move || {
                serve_connection(
                    1,
                    server,
                    tx,
                    writers,
                    Arc::new(std::sync::atomic::AtomicBool::new(false)),
                );
            });
            let mut frame =
                serde_json::to_vec(&serde_json::json!({"protocol": protocol, "app_id": app_id}))
                    .expect("serialize invalid handshake");
            frame.push(b'\n');
            client.write_all(&frame).expect("write invalid handshake");
            drop(client);
            worker.join().expect("service worker exits");
            assert!(
                rx.try_recv().is_err(),
                "refused launch emitted an authenticated event"
            );
            assert!(
                observed.lock().expect("writers").is_empty(),
                "refused launch retained a writer"
            );
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn drop_stops_acceptor_promptly_and_unlinks_socket() {
        let runtime = tempfile::tempdir().expect("temporary runtime");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", runtime.path());

        let socket = launcher::services::socket_for(FLOATING_SURFACE_SERVICE);
        let service = FloatingSurfaceService::start().expect("start floating service");
        assert!(
            socket.exists(),
            "service must publish its socket while live"
        );
        let started = Instant::now();
        drop(service);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "dropping the service must not wait on an accept() blocked forever"
        );
        assert!(!socket.exists(), "drop must unlink the service socket");
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial_test::serial(process_env)]
    fn idle_acceptor_sleeps_until_a_client_arrives() {
        let runtime = tempfile::tempdir().expect("temporary runtime");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", runtime.path());

        let (service, acceptor) = tilo_test_env::idle::spawned_thread("tilo-shell-ui-a", || {
            FloatingSurfaceService::start().expect("start floating service")
        });
        tilo_test_env::idle::assert_stays_asleep(&acceptor, "shell-ui acceptor");
        UnixStream::connect(launcher::services::socket_for(FLOATING_SURFACE_SERVICE))
            .expect("still accepts clients");
        drop(service);
    }
}
