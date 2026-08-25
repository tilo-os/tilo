#![allow(dead_code)]

pub mod offscreen;

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
/// event to every other peer. Sender never receives its own broadcast.
/// - `SyncIncremental` → `CRDTIncremental`
/// - `SyncCRDT` → `CRDTUpdate`
/// - `Presence` → `RemotePresenceUpdate` (attributed to the sender's id,
///    so the receiving peer knows *who* the cursor/caret belongs to)
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
                // with an empty, exhausted batch (Phase 6 state is
                // broker-side, mirrored from collaboration.rs).
                if let Some(inbox) = inboxes.get_mut(&from) {
                    inbox.push_back(CollabEvent::ScrollbackBatch {
                        artifact: artifact.clone(),
                        ops: Vec::new(),
                        exhausted: true,
                    });
                }
            }
            CollabRequest::OfferBlob { hash, bytes } => {
                // Content one peer stored reaches the others as content that
                // arrived — the same translation the real broker makes, so a
                // document's picture crosses this relay the way it crosses a
                // room.
                for id in &peer_ids {
                    if id == &from {
                        continue;
                    }
                    inboxes
                        .get_mut(id)
                        .unwrap()
                        .push_back(CollabEvent::BlobArrived {
                            hash: hash.clone(),
                            bytes: bytes.clone(),
                        });
                }
            }
            CollabRequest::WantBlob { .. } => {
                // Answered by whoever holds the content offering it; this
                // relay has no store of its own to answer from.
            }
            CollabRequest::SendAction { type_id, payload } => {
                // An action published into the room reaches every other peer,
                // which is what the real broker does with one. It is narrowed
                // at the *receiver* — by whether that peer's own registry
                // knows the type — so a relay that narrowed it here would be
                // answering a question the registry already answers, and
                // would hide the case where the wrong app is listening.
                for id in &peer_ids {
                    if id == &from {
                        continue;
                    }
                    inboxes
                        .get_mut(id)
                        .unwrap()
                        .push_back(CollabEvent::ActionArrived {
                            type_id: type_id.clone(),
                            payload: payload.clone(),
                            from_user_id: from.clone(),
                        });
                }
            }
            CollabRequest::OpenApp { .. } => {
                // Addressed to a session rather than to an audience, and this
                // relay hosts no desktop to address.
            }
            CollabRequest::Hello { .. } => {
                // no fan-out in tests
            }
            // Addressed to the session hosting a connection rather than to the
            // room, so the real broker takes it out of the fan-out and hands it
            // to a desktop, refusing it when there is none. This relay is a map
            // of inboxes in one process: it hosts nothing, so it can neither
            // open the application nor refuse on a session's behalf, and fanning
            // it out would hand every other peer a request meant for one of them.
            //
            // No test here sends one. Arriving means the harness is being asked
            // for something it does not implement, and `pump` runs on the test
            // thread, so saying so fails that test by name — where a frame left
            // to vanish would let a later assertion pass for the wrong reason.
            CollabRequest::OpenApp { app_id } => panic!(
                "MemoryRelay::pump got OpenApp({app_id:?}) from {from}: this \
                 relay fans document and presence traffic between in-process \
                 peers and hosts no desktop, so it cannot open an application. \
                 A test that needs one must give the harness an opener, the way \
                 the shell's broker takes an `AppOpener`."
            ),
            // Same reading, and the same reason to say it out loud: a report
            // that content crossed between two applications is addressed to
            // the session that draws them, and this one draws nothing.
            CollabRequest::CarriedContent { from_app, into_app, .. } => panic!(
                "MemoryRelay::pump got CarriedContent({from_app:?} -> \
                 {into_app:?}) from {from}: this relay hosts no windows, so a \
                 crossing between two applications has nowhere to be drawn."
            ),
            // Same reading again: a compose surface is drawn by the session
            // that owns the desktop, and this relay owns none.
            CollabRequest::ComposeRemark { reference, .. } => panic!(
                "MemoryRelay::pump got ComposeRemark({reference:?}) from \
                 {from}: this relay draws no windows, so there is nowhere for \
                 a compose surface to sit."
            ),
            // What an app can do is published into a session's catalogue, and
            // this relay keeps none. A test that wants to assert on a
            // published surface should drive the shell's broker, where the
            // catalogue actually is.
            CollabRequest::PublishActions { app_id, .. } => panic!(
                "MemoryRelay::pump got PublishActions({app_id:?}) from \
                 {from}: this relay has no catalogue to publish into."
            ),
            CollabRequest::CompleteInvocation { id, .. } => panic!(
                "MemoryRelay::pump got CompleteInvocation({id}) from {from}: \
                 nothing on this relay can have asked for one."
            ),
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
