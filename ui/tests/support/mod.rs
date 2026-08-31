#![allow(dead_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

pub mod offscreen;
pub mod typed_collab;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use ui::App;
use ui::collaboration::{CollabEvent, CollabRequest};
use ui::state::State;
use ui::widget::{Column, Text};

/// An in-memory stand-in for the Unix-socket `CollaborationManager`.
/// Each `Peer` exposes `send(req)` and `poll() -> Vec<CollabEvent>`.
///
/// Usage:
///     let mut relay = `MemoryRelay::new()`;
///     let alice = relay.attach("alice");
///     let bob   = relay.attach("bob");
///     `alice.send(CollabRequest::SyncIncremental` { data: vec![1,2,3] });
///     `relay.drain_peer_outbox(&alice)`;
///     pump(&mut relay);
///     let events = `bob.poll()`;
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

    pub fn attach(&self, id: &str) -> Peer {
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
        self.inbox
            .lock()
            .unwrap()
            .get_mut(&self.id)
            .expect("peer inbox removed from relay")
            .drain(..)
            .collect()
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
/// event to every other peer. Sender never receives its own broadcast.
/// - `SyncIncremental` → `CRDTIncremental`
/// - `SyncCRDT` → `CRDTUpdate`
/// - `Presence` → `RemotePresenceUpdate` (attributed to the sender's id,
///   so the receiving peer knows *who* the cursor/caret belongs to)
/// - `Hello` is not fanned out
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
            CollabRequest::Presence { key, payload } => {
                for id in &peer_ids {
                    if id == &from {
                        continue;
                    }
                    inboxes
                        .get_mut(id)
                        .unwrap()
                        .push_back(CollabEvent::RemotePresenceUpdate {
                            user_id: from.clone(),
                            key: key.clone(),
                            payload: payload.clone(),
                        });
                }
            }
            CollabRequest::SignedSyncIncremental { envelope } => {
                // Mirror the production fan-out (collaboration.rs): the
                // in-toolkit test broker routes verbatim (no role
                // enforcement — that's the shell collab_broker's job).
                for id in &peer_ids {
                    if id == &from {
                        continue;
                    }
                    inboxes
                        .get_mut(id)
                        .unwrap()
                        .push_back(CollabEvent::SignedCRDTIncremental {
                            envelope: envelope.clone(),
                            from_user_id: from.clone(),
                        });
                }
            }
            CollabRequest::FetchScrollback { artifact, .. } => {
                // The test broker holds no op log — answer the requester
                // with an empty, exhausted batch.
                if let Some(inbox) = inboxes.get_mut(&from) {
                    inbox.push_back(CollabEvent::ScrollbackBatch {
                        artifact,
                        ops: Vec::new(),
                        exhausted: true,
                    });
                }
            }
            CollabRequest::Hello { .. } => {
                // no fan-out in tests
            }
        }
    }
}

/// Build a minimal headless `App` paired with a `Peer` for integration tests.
///
/// The returned `App` is not driven by a real `winit` event loop; it has a
/// minimal widget tree (`Column` with a single `Text`) and is primarily used
/// to drive `apply_collab_event_for_test` / `poll_and_apply_collab_events`
/// against `presence_ctx` and `permission_ctx`.
///
/// The second `State` value is returned so the test can construct mutations
/// independently when it needs to (e.g. to stage CRDT deltas).
pub fn headless_app(_user: &str, _peer: Peer) -> (App, State) {
    let state = State::new();
    let root = Box::new(Column::new().push(Box::new(Text::new("hello"))));
    let app = App::new(state, root);
    let state2 = State::new();
    (app, state2)
}
