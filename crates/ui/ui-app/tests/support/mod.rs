#![allow(
    dead_code,
    reason = "each integration test compiles this shared support module but uses only some helpers"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

pub mod evidence;
pub mod map;
pub mod render;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tilo_ui_app::App;
use tilo_ui_app::state::State;
use tilo_ui_app::widget::{Column, Text};
use tilo_ui_base::{collaboration::CollabEvent, collaboration::CollabRequest};

type PeerInboxes = HashMap<String, VecDeque<CollabEvent>>;

/// An in-memory stand-in for the Unix-socket `CollaborationManager`.
/// Each `Peer` exposes `send(req)` and `poll() -> Vec<CollabEvent>`.
///
/// Usage:
///     let mut relay = `MemoryRelay::new()`;
///     let alice = relay.attach("alice");
///     let bob   = relay.attach("bob");
///     `alice.send(CollabRequest::document_incremental(vec![1,2,3]));`
///     `relay.drain_peer_outbox(&alice)`;
///     pump(&mut relay);
///     let events = `bob.poll()`;
pub struct MemoryRelay {
    pending_out: Vec<(String, CollabRequest)>,
    inboxes: Arc<Mutex<PeerInboxes>>,
}

#[derive(Clone)]
pub struct Peer {
    id: String,
    outbox: Arc<Mutex<VecDeque<CollabRequest>>>,
    inbox: Arc<Mutex<PeerInboxes>>,
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
/// Document commands arrive as the changes of the shared route's record, as
/// consumers receive them from the device's store.
/// - `Presence` → `RemotePresenceUpdate` (attributed to the sender's id,
///   so the receiving peer knows *who* the cursor/caret belongs to)
/// - `Hello` is not fanned out
pub fn pump(relay: &mut MemoryRelay) {
    let pending = std::mem::take(&mut relay.pending_out);
    let mut inboxes = relay.inboxes.lock().unwrap();
    let peer_ids: Vec<String> = inboxes.keys().cloned().collect();
    for (from, req) in pending {
        match req {
            CollabRequest::Document { mutation, .. } => {
                let event = CollabEvent::Document {
                    route: tilo_ui_base::collaboration::RouteSpec::cluster(uuid::Uuid::nil()),
                    mutation,
                };
                broadcast_to_others(&mut inboxes, &peer_ids, &from, &event);
            }
            CollabRequest::OpenWorkspaceControl { session_id, .. } => {
                refuse_workspace_control(&mut inboxes, &from, session_id);
            }
            CollabRequest::ReportLocalDocumentAction { request_id, .. } => {
                refuse_local_document_action(&mut inboxes, &from, request_id);
            }
            CollabRequest::WorkspaceControlInput { .. }
            | CollabRequest::OfferContent { .. }
            | CollabRequest::WantContent { .. }
            | CollabRequest::CloseWorkspaceControl { .. }
            | CollabRequest::Hello { .. }
            | CollabRequest::PublishActionSurface { .. }
            | CollabRequest::ReportContinuation { .. }
            | CollabRequest::PublishSearchIndex { .. }
            | CollabRequest::ClearContinuation
            | CollabRequest::ReportHandoff { .. }
            | CollabRequest::UpdateInvocation { .. }
            | CollabRequest::ResourceLocalEffect { .. }
            | CollabRequest::RemoteAction { .. }
            | CollabRequest::ResourceCommand { .. }
            | CollabRequest::DecideShownActions { .. }
            | CollabRequest::ExternalModeration { .. } => {
                // Session-local and shell-owned control-plane commands never
                // fan out through another application's document channel. A
                // moderation request has no host authority here: dropping its
                // response ownership refuses it.
            }
        }
    }
    drop(inboxes);
}

fn refuse_workspace_control(inboxes: &mut PeerInboxes, from: &str, session_id: u64) {
    // This harness has no authenticated shell/network boundary or consent.
    if let Some(inbox) = inboxes.get_mut(from) {
        inbox.push_back(CollabEvent::WorkspaceControl {
            session_id,
            event: runtime_collab::WorkspaceControlEvent::Closed(
                runtime_collab::WorkspaceControlCloseReason::Denied(
                    "memory relay has no workspace-control transport".into(),
                ),
            ),
        });
    }
}

/// Answer a local document action this relay has no authority for with its refusal, to the
/// requesting peer alone.
fn refuse_local_document_action(inboxes: &mut PeerInboxes, from: &str, request_id: u64) {
    if let Some(inbox) = inboxes.get_mut(from) {
        inbox.push_back(CollabEvent::LocalDocumentActionReported {
            request_id,
            result: Err(runtime_collab::LocalDocumentActionError::Rejected(
                "memory relay has no authenticated admission authority".into(),
            )),
        });
    }
}

fn broadcast_to_others(
    inboxes: &mut PeerInboxes,
    peer_ids: &[String],
    sender: &str,
    event: &CollabEvent,
) {
    for id in peer_ids.iter().filter(|id| id.as_str() != sender) {
        inboxes
            .get_mut(id)
            .expect("attached peer retains an inbox")
            .push_back(event.clone());
    }
}

pub fn document_mutation(event: &CollabEvent) -> Option<collab_types::DocumentMutation> {
    event.document_mutation().cloned()
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

pub mod application;
