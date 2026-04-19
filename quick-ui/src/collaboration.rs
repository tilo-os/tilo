use std::os::unix::net::UnixStream;
use std::io::{Read, Write};
use serde::{Serialize, Deserialize};
use uuid::Uuid;

use crate::presence::PresenceKey;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabRequest {
    Hello { app_id: String, cluster_id: Uuid },
    SyncCRDT { data: Vec<u8> },
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
    RemotePresenceUpdate {
        user_id: String,
        key: PresenceKey,
        payload: PresencePayload,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
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
