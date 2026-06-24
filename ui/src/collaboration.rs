use std::os::unix::net::UnixStream;
use std::io::{Read, Write};
use serde::{Serialize, Deserialize};
use uuid::Uuid;

use crate::presence::PresenceKey;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabRequest {
    Hello { app_id: String, cluster_id: Uuid },
    SyncCRDT { data: Vec<u8> },
    SyncIncremental { data: Vec<u8> },
    /// Outbound presence update — the local user's pointer moved, caret
    /// changed, selection shifted, or they arrived/left a keyed object.
    /// The server fans this out to every connected peer as a
    /// `CollabEvent::RemotePresenceUpdate`; the test relay uses the same
    /// path. Round-trips complete the loop with `apply_remote_presence`.
    Presence { key: PresenceKey, payload: PresencePayload },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabEvent {
    Context {
        collaborators: Vec<Collaborator>,
        permissions: Vec<String>,
    },
    RemoteCursorMoved {
        user_id: String,
        x: f32,
        y: f32,
    },
    CRDTUpdate {
        data: Vec<u8>,
    },
    CRDTIncremental {
        data: Vec<u8>,
    },
    RemotePresenceUpdate {
        user_id: String,
        key: PresenceKey,
        payload: PresencePayload,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum PresencePayload {
    Pointer { x: f32, y: f32 },
    Caret { pos: usize },
    Selection { start: usize, end: usize },
    Arrived,
    Left,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    pub color: [f32; 3],
}

pub struct CollaborationManager {
    stream: Option<UnixStream>,
}

impl CollaborationManager {
    pub fn new() -> Self {
        let stream = if let Ok(path) = std::env::var("QUICK_COLLAB_SOCK") {
            UnixStream::connect(path).ok()
        } else {
            None
        };

        let mut manager = Self { stream };
        
        if manager.stream.is_some() {
            let app_id = std::env::var("QUICK_APP_ID").unwrap_or_else(|_| Uuid::new_v4().to_string());
            let cluster_id = std::env::var("QUICK_CLUSTER_ID")
                .ok()
                .and_then(|s| Uuid::parse_str(&s).ok())
                .unwrap_or_else(Uuid::nil);

            manager.send(CollabRequest::Hello { 
                app_id,
                cluster_id,
            });
        }

        manager
    }

    pub fn send(&mut self, req: CollabRequest) {
        if let Some(ref mut s) = self.stream {
            if let Ok(json) = serde_json::to_vec(&req) {
                let len = json.len() as u32;
                let _ = s.write_all(&len.to_le_bytes());
                let _ = s.write_all(&json);
            }
        }
    }

    pub fn poll(&mut self) -> Vec<CollabEvent> {
        let mut events = Vec::new();
        if let Some(ref mut s) = self.stream {
            let _ = s.set_nonblocking(true);
            loop {
                let mut len_buf = [0u8; 4];
                if let Err(e) = s.read_exact(&mut len_buf) {
                    if e.kind() == std::io::ErrorKind::WouldBlock { break; }
                    break;
                }
                let len = u32::from_le_bytes(len_buf) as usize;
                let mut buf = vec![0u8; len];
                if s.read_exact(&mut buf).is_err() { break; }
                if let Ok(event) = serde_json::from_slice(&buf) {
                    events.push(event);
                }
            }
        }
        events
    }
}

/// Standalone N-peer fan-out broker for `QUICK_COLLAB_SOCK`.
///
/// This is the production counterpart to the test relay in
/// `ui/tests/real_socket_two_peer.rs`, generalized from a fixed two
/// peers to any number that connect over the lifetime of the process.
/// It carries no Automerge knowledge — it simply relays opaque frames:
///
///   * `Hello` registers the connection's `user_id` (from `app_id`) and
///     is then swallowed (never fanned out).
///   * `SyncIncremental` → `CRDTIncremental`, `SyncCRDT` → `CRDTUpdate`
///     are forwarded verbatim to every *other* connected peer.
///   * `Presence` → `RemotePresenceUpdate` is forwarded to every other
///     peer, attributed with the sender's `user_id`.
///
/// Late-join document convergence is handled at the app layer (a new
/// peer broadcasts `Presence::Arrived`; peers already in the room reply
/// with a full `SyncCRDT`) — the broker needs no special case for it.
pub mod broker {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::{CollabEvent, CollabRequest};

    fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        s.read_exact(&mut len_buf).ok()?;
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        s.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    fn write_frame(s: &mut UnixStream, bytes: &[u8]) {
        let len = bytes.len() as u32;
        let _ = s.write_all(&len.to_le_bytes());
        let _ = s.write_all(bytes);
    }

    /// A connected peer's write side + attributed identity.
    struct Peer {
        user_id: String,
        write: UnixStream,
    }

    /// Bind `path` (removing any stale inode first) and serve forever.
    /// Convenience entry point for the `collab-broker` binary.
    pub fn serve_path<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
        let path = path.as_ref();
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        serve(listener);
        Ok(())
    }

    /// Serve on an already-bound listener until it stops yielding
    /// connections. Blocks the calling thread; spawns one reader thread
    /// per peer plus an accept thread.
    pub fn serve(listener: UnixListener) {
        let peers: Arc<Mutex<HashMap<usize, Peer>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (fwd_tx, fwd_rx) = mpsc::channel::<(usize, Vec<u8>)>();

        // Accept connections on a side thread so the forwarding loop can
        // own the calling thread. Each connection gets its own reader
        // thread so a silent peer can't stall accept for the others.
        let accept_peers = Arc::clone(&peers);
        thread::spawn(move || {
            let mut next_idx = 0usize;
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { continue };
                let idx = next_idx;
                next_idx += 1;
                let tx = fwd_tx.clone();
                let peers = Arc::clone(&accept_peers);
                thread::spawn(move || {
                    let Ok(write) = stream.try_clone() else { return };
                    // First frame establishes identity. The client sends
                    // `Hello` immediately on connect; if it doesn't, fall
                    // back to `peer-{idx}` and treat the frame as data.
                    match read_frame(&mut stream) {
                        Some(frame) => match serde_json::from_slice::<CollabRequest>(&frame) {
                            Ok(CollabRequest::Hello { app_id, .. }) => {
                                peers.lock().unwrap().insert(
                                    idx,
                                    Peer { user_id: app_id, write },
                                );
                            }
                            _ => {
                                peers.lock().unwrap().insert(
                                    idx,
                                    Peer { user_id: format!("peer-{idx}"), write },
                                );
                                let _ = tx.send((idx, frame));
                            }
                        },
                        None => return, // closed before saying anything
                    }

                    while let Some(frame) = read_frame(&mut stream) {
                        if tx.send((idx, frame)).is_err() {
                            break;
                        }
                    }
                    // Peer closed — stop writing to it.
                    peers.lock().unwrap().remove(&idx);
                });
            }
        });

        // Forwarding loop. Never returns while the accept thread holds a
        // `fwd_tx` clone (i.e. for the life of the process).
        while let Ok((src_idx, frame)) = fwd_rx.recv() {
            let Ok(req) = serde_json::from_slice::<CollabRequest>(&frame) else {
                continue;
            };

            let user_id = peers
                .lock()
                .unwrap()
                .get(&src_idx)
                .map(|p| p.user_id.clone())
                .unwrap_or_else(|| format!("peer-{src_idx}"));

            let event = match req {
                CollabRequest::Hello { .. } => continue,
                CollabRequest::SyncIncremental { data } => {
                    CollabEvent::CRDTIncremental { data }
                }
                CollabRequest::SyncCRDT { data } => {
                    CollabEvent::CRDTUpdate { data }
                }
                CollabRequest::Presence { key, payload } => {
                    CollabEvent::RemotePresenceUpdate { user_id, key, payload }
                }
            };

            let Ok(bytes) = serde_json::to_vec(&event) else { continue };

            let mut guard = peers.lock().unwrap();
            for (idx, peer) in guard.iter_mut() {
                if *idx != src_idx {
                    write_frame(&mut peer.write, &bytes);
                }
            }
        }
    }
}
