#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use quick_ui::collaboration::{CollabEvent, CollabRequest};

/// An in-memory stand-in for the Unix-socket `CollaborationManager`.
/// Each `Peer` exposes `send(req)` and `poll() -> Vec<CollabEvent>`.
///
/// Usage:
///     let mut relay = MemoryRelay::new();
///     let alice = relay.attach("alice");
///     let bob   = relay.attach("bob");
///     alice.send(CollabRequest::SyncIncremental { data: vec![1,2,3] });
///     relay.drain_peer_outbox(&alice);
///     pump(&mut relay);
///     let events = bob.poll();
pub struct MemoryRelay {
    pending_out: Vec<(String, CollabRequest)>,
    inboxes: Arc<Mutex<HashMap<String, VecDeque<CollabEvent>>>>,
}

#[derive(Clone)]
pub struct Peer {
    id: String,
    outbox: Arc<Mutex<VecDeque<CollabRequest>>>,
    inbox: Arc<Mutex<HashMap<String, VecDeque<CollabEvent>>>>,
}

impl MemoryRelay {
    pub fn new() -> Self {
        Self {
            pending_out: Vec::new(),
            inboxes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn attach(&mut self, id: &str) -> Peer {
        self.inboxes
            .lock()
            .unwrap()
            .insert(id.to_string(), VecDeque::new());
        Peer {
            id: id.to_string(),
            outbox: Arc::new(Mutex::new(VecDeque::new())),
            inbox: self.inboxes.clone(),
        }
    }

    /// Move every currently-queued request in `peer`'s outbox into the
    /// relay's pending list. Call before `pump`.
    pub fn drain_peer_outbox(&mut self, peer: &Peer) {
        let mut out = peer.outbox.lock().unwrap();
        while let Some(req) = out.pop_front() {
            self.pending_out.push((peer.id.clone(), req));
        }
    }
}

impl Default for MemoryRelay {
    fn default() -> Self {
        Self::new()
    }
}

impl Peer {
    pub fn send(&self, req: CollabRequest) {
        self.outbox.lock().unwrap().push_back(req);
    }

    pub fn poll(&self) -> Vec<CollabEvent> {
        let mut guard = self.inbox.lock().unwrap();
        let mine = guard
            .get_mut(&self.id)
            .expect("peer inbox removed from relay");
        mine.drain(..).collect()
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Direct inject to self. Used by test helpers that want to model
    /// fan-out without wiring it through `pump`.
    pub fn inject(&self, event: CollabEvent) {
        self.inbox
            .lock()
            .unwrap()
            .get_mut(&self.id)
            .expect("peer inbox removed from relay")
            .push_back(event);
    }
}

/// Process all pending outbound requests: translate each into a broadcast
/// event to every other peer. Presence + incremental updates are forwarded
/// verbatim; `SyncIncremental` is rebroadcast as `CRDTIncremental`, and
/// `SyncCRDT` as `CRDTUpdate`. `Hello` is not fanned out.
pub fn pump(relay: &mut MemoryRelay) {
    let pending = std::mem::take(&mut relay.pending_out);
    let mut inboxes = relay.inboxes.lock().unwrap();
    let peer_ids: Vec<String> = inboxes.keys().cloned().collect();
    for (from, req) in pending {
        match req {
            CollabRequest::SyncIncremental { data } => {
                for id in &peer_ids {
                    if id == &from {
                        continue;
                    }
                    inboxes
                        .get_mut(id)
                        .unwrap()
                        .push_back(CollabEvent::CRDTIncremental { data: data.clone() });
                }
            }
            CollabRequest::SyncCRDT { data } => {
                for id in &peer_ids {
                    if id == &from {
                        continue;
                    }
                    inboxes
                        .get_mut(id)
                        .unwrap()
                        .push_back(CollabEvent::CRDTUpdate { data: data.clone() });
                }
            }
            CollabRequest::Hello { .. } => {
                // no fan-out in tests
            }
        }
    }
}
