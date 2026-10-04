//! Authenticated app ↔ compositor lifecycle channel for floating surfaces.
//!
//! The launcher opens the socket and writes the canonical app id before the
//! sandbox starts. Only after that handshake is accepted do client lifecycle
//! messages enter the compositor. This prevents a sandboxed app from naming a
//! different app id and turning `PiP` into a cross-app capture primitive.

use serde::Deserialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use launcher::services::SHELL_UI_HANDSHAKE_VERSION;
use ui::floating_surface::{
    FLOATING_SURFACE_SERVICE, FloatingSurfaceClientMessage, FloatingSurfaceHostMessage,
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
    },
    Message {
        connection: u64,
        app_id: String,
        message: FloatingSurfaceClientMessage,
    },
    Disconnected {
        connection: u64,
        app_id: String,
    },
}

#[derive(Deserialize)]
struct LauncherHandshake {
    protocol: u16,
    app_id: String,
}

pub struct FloatingSurfaceService {
    events: mpsc::Receiver<FloatingServiceEvent>,
    writers: Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>>,
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
                            let _ = std::thread::Builder::new()
                                .name(format!("tilo-shell-ui-{connection}"))
                                .spawn(move || serve_connection(connection, stream, tx, writers));
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

    pub fn send(&self, connection: u64, message: &FloatingSurfaceHostMessage) {
        let Ok(mut frame) = serde_json::to_vec(message) else {
            return;
        };
        frame.push(b'\n');
        let writer = self
            .writers
            .lock()
            .ok()
            .and_then(|writers| writers.get(&connection).cloned());
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
                    .map(|(connection, writer)| (*connection, writer.clone()))
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
    writers: Arc<Mutex<HashMap<u64, mpsc::Sender<Vec<u8>>>>>,
) {
    let Ok(mut writer) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(stream);
    let Some(handshake_frame) = read_frame(&mut reader).ok().flatten() else {
        return;
    };
    let Ok(handshake) = serde_json::from_slice::<LauncherHandshake>(&handshake_frame) else {
        return;
    };
    if handshake.protocol != SHELL_UI_HANDSHAKE_VERSION
        || handshake.app_id.trim().is_empty()
        || handshake.app_id.len() > 255
    {
        return;
    }
    let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>();
    if let Ok(mut open) = writers.lock() {
        open.insert(connection, write_tx);
    } else {
        return;
    }
    let _ = std::thread::Builder::new()
        .name(format!("tilo-shell-ui-writer-{connection}"))
        .spawn(move || {
            for frame in write_rx {
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
    let _ = events.send(FloatingServiceEvent::Connected {
        connection,
        app_id: app_id.clone(),
    });

    loop {
        let frame = match read_frame(&mut reader) {
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

/// What each authenticated app was last told about where, and for whom,
/// its window is shown ([`ui::WindowAppearance`]), so it is told again only
/// when that changes.
#[derive(Debug, Default)]
pub struct AppearanceDelivery {
    told: HashMap<u64, (String, Option<ui::WindowAppearance>)>,
}

impl AppearanceDelivery {
    /// `app_id` connected on `connection`; it has been told nothing yet.
    pub fn connected(&mut self, connection: u64, app_id: &str) {
        self.told.insert(connection, (app_id.to_owned(), None));
    }

    pub fn disconnected(&mut self, connection: u64) {
        self.told.remove(&connection);
    }

    /// Tell every connection whose window's appearance (`appearance_of` its
    /// app id) differs from what it was last told.
    pub fn publish(
        &mut self,
        appearance_of: impl Fn(&str) -> ui::WindowAppearance,
        mut send: impl FnMut(u64, &FloatingSurfaceHostMessage),
    ) {
        for (connection, (app_id, told)) in &mut self.told {
            let now = appearance_of(app_id);
            if *told != Some(now) {
                send(*connection, &FloatingSurfaceHostMessage::appearance(&now));
                *told = Some(now);
            }
        }
    }
}

fn read_frame(reader: &mut BufReader<UnixStream>) -> std::io::Result<Option<Vec<u8>>> {
    let mut frame = Vec::new();
    let count = reader.read_until(b'\n', &mut frame)?;
    if count == 0 {
        return Ok(None);
    }
    if frame.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "shell-ui frame exceeds 1 MiB",
        ));
    }
    if frame.last() != Some(&b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "shell-ui frame has no newline terminator",
        ));
    }
    frame.pop();
    Ok(Some(frame))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

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
        delivery.connected(1, "so.tilo.notes");
        delivery.connected(2, "so.tilo.files");
        let mut sent = Vec::new();
        delivery.publish(appearance_of, |connection, message| {
            sent.push((connection, message.window_appearance()));
        });
        sent.sort_by_key(|(connection, _)| *connection);
        assert_eq!(sent, vec![(1, Some(warm)), (2, Some(grey))]);

        sent.clear();
        delivery.publish(appearance_of, |connection, message| {
            sent.push((connection, message.window_appearance()));
        });
        assert!(sent.is_empty(), "nothing changed, nothing is said");

        rooms.set((grey, grey));
        delivery.publish(appearance_of, |connection, message| {
            sent.push((connection, message.window_appearance()));
        });
        assert_eq!(sent, vec![(1, Some(grey))], "only the window that moved");

        sent.clear();
        delivery.disconnected(1);
        rooms.set((warm, warm));
        delivery.publish(appearance_of, |connection, message| {
            sent.push((connection, message.window_appearance()));
        });
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
            "protocol": SHELL_UI_HANDSHAKE_VERSION,
            "app_id": "so.tilo.notes",
        }))
        .unwrap();
        handshake.push(b'\n');
        app.write_all(&handshake).unwrap();
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

        let (service, acceptor) =
            crate::idle_thread_probe::spawned_thread("tilo-shell-ui-a", || {
                FloatingSurfaceService::start().expect("start floating service")
            });
        crate::idle_thread_probe::assert_stays_asleep(&acceptor, "shell-ui acceptor");
        UnixStream::connect(launcher::services::socket_for(FLOATING_SURFACE_SERVICE))
            .expect("still accepts clients");
        drop(service);
    }
}
