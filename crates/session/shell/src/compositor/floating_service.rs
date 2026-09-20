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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use ui::floating_surface::{
    FLOATING_SURFACE_PROTOCOL_VERSION, FLOATING_SURFACE_SERVICE, FloatingSurfaceClientMessage,
    FloatingSurfaceHostMessage,
};

const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
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
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("make shell-ui listener nonblocking: {error}"))?;

        let (event_tx, event_rx) = mpsc::channel();
        let writers = Arc::new(Mutex::new(HashMap::new()));
        let thread_writers = Arc::clone(&writers);
        std::thread::Builder::new()
            .name("qos-shell-ui-accept".into())
            .spawn(move || {
                let next_id = AtomicU64::new(1);
                loop {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let connection = next_id.fetch_add(1, Ordering::Relaxed);
                            let tx = event_tx.clone();
                            let writers = Arc::clone(&thread_writers);
                            let _ = std::thread::Builder::new()
                                .name(format!("qos-shell-ui-{connection}"))
                                .spawn(move || serve_connection(connection, stream, tx, writers));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(12));
                        }
                        Err(error) => {
                            tracing::warn!(%error, "shell-ui service accept failed");
                            std::thread::sleep(Duration::from_millis(100));
                        }
                    }
                }
            })
            .map_err(|error| format!("spawn shell-ui service: {error}"))?;

        Ok(Self {
            events: event_rx,
            writers,
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

// These channel handles are deliberately owned by the per-connection worker,
// which can outlive the accept-loop stack frame.
#[allow(clippy::needless_pass_by_value)]
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
    if handshake.protocol != FLOATING_SURFACE_PROTOCOL_VERSION
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
        .name(format!("qos-shell-ui-writer-{connection}"))
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
