//! Session service for typed, contextual launches from sandboxed system UI.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use os_actions::{TOOL_PROTOCOL_VERSION, ToolReply, ToolRequest};

pub const TOOL_SERVICE: &str = "tools";
const MAX_WIRE_BYTES: u64 = 4 * 1024;

type Handler = Arc<dyn Fn(ToolRequest) -> Result<(), String> + Send + Sync>;

pub struct ToolServiceHandle {
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    path: PathBuf,
}

impl Drop for ToolServiceHandle {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // Wake the nonblocking listener immediately rather than waiting for a
        // scheduling tick during compositor shutdown.
        let _ = UnixStream::connect(&self.path);
        if let Some(thread) = self.thread.take() {
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
    spawn_at(
        path,
        Arc::new(|request| {
            let room = crate::shell::state::RoomId::parse(&request.workspace_id)
                .ok_or_else(|| "invalid workspace identity".to_string())?;
            request.intent.validate()?;
            crate::shell::launcher::launch_tool_intent(&request.intent, room)
                .map(|_| ())
                .map_err(|e| e.user_message())
        }),
    )
}

fn spawn_at(path: PathBuf, handler: Handler) -> std::io::Result<ToolServiceHandle> {
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
    listener.set_nonblocking(true)?;
    let shutdown = Arc::new(AtomicBool::new(false));
    let worker_shutdown = Arc::clone(&shutdown);
    let thread = std::thread::Builder::new()
        .name("qos-tool-intents".into())
        .spawn(move || {
            while !worker_shutdown.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let handler = Arc::clone(&handler);
                        let _ = std::thread::Builder::new()
                            .name("qos-tool-client".into())
                            .spawn(move || serve_client(stream, &handler));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "tool-intent listener failed");
                        break;
                    }
                }
            }
        })?;
    Ok(ToolServiceHandle {
        shutdown,
        thread: Some(thread),
        path,
    })
}

fn serve_client(stream: UnixStream, handler: &Handler) {
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
            }
        } else {
            match serde_json::from_str::<ToolRequest>(&line) {
                Ok(request) if request.protocol == TOOL_PROTOCOL_VERSION => {
                    match handler(request) {
                        Ok(()) => ToolReply {
                            accepted: true,
                            message: "opened".into(),
                        },
                        Err(message) => ToolReply {
                            accepted: false,
                            message,
                        },
                    }
                }
                Ok(_) => ToolReply {
                    accepted: false,
                    message: "unsupported tool protocol".into(),
                },
                Err(_) => ToolReply {
                    accepted: false,
                    message: "invalid tool request".into(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use os_actions::{ToolIntent, VpnRoute};
    use std::sync::Mutex;

    #[test]
    fn service_accepts_only_versioned_typed_requests() {
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
}
