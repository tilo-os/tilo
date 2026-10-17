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
pub mod ratchet;
#[cfg(unix)]
pub mod typed_collab;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tilo_ui::App;
use tilo_ui::collaboration::{CollabEvent, CollabRequest};
use tilo_ui::state::State;
use tilo_ui::widget::{Column, Text};

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
/// Document commands are wrapped in the same typed signed-envelope shape
/// consumers receive from the production broker.
/// - `Presence` → `RemotePresenceUpdate` (attributed to the sender's id,
///   so the receiving peer knows *who* the cursor/caret belongs to)
/// - `Hello` is not fanned out
pub fn pump(relay: &mut MemoryRelay) {
    let pending = std::mem::take(&mut relay.pending_out);
    let mut inboxes = relay.inboxes.lock().unwrap();
    let peer_ids: Vec<String> = inboxes.keys().cloned().collect();
    for (from, req) in pending {
        match req {
            #[cfg(feature = "broker-transport")]
            CollabRequest::ExternalModeration { .. } => {
                // No host authority here: dropping response custody refuses the request.
            }
            CollabRequest::Document { mutation, .. } => {
                let mutation = relay_mutation(mutation);
                let event = document_event(&mutation, &from);
                broadcast_to_others(&mut inboxes, &peer_ids, &from, &event);
            }
            CollabRequest::Presence { key, payload } => {
                let event = CollabEvent::RemotePresenceUpdate {
                    user_id: from.clone(),
                    key,
                    payload,
                };
                broadcast_to_others(&mut inboxes, &peer_ids, &from, &event);
            }
            CollabRequest::SignedOperation { envelope } => {
                // Mirror the production fan-out (collaboration.rs): the
                // in-toolkit test broker routes verbatim (no role
                // enforcement — that's the shell collab_broker's job).
                let event = CollabEvent::Document {
                    envelope,
                    from_user_id: from.clone(),
                    decrypted_payload: None,
                };
                broadcast_to_others(&mut inboxes, &peer_ids, &from, &event);
            }
            CollabRequest::FetchScrollback { artifact, .. } => {
                respond_with_empty_scrollback(&mut inboxes, &from, artifact);
            }
            CollabRequest::RequestDocumentSnapshot => {
                request_document_snapshot(&mut inboxes, &peer_ids, &from);
            }
            CollabRequest::OpenWorkspaceControl { session_id, .. } => {
                refuse_workspace_control(&mut inboxes, &from, session_id);
            }
            CollabRequest::SelectedDiscovery { request_id, .. } => {
                if let Some(inbox) = inboxes.get_mut(&from) {
                    inbox.push_back(CollabEvent::SelectedDiscoveryResult {
                        request_id,
                        result: Err("memory relay has no selected-file discovery".into()),
                    });
                }
            }
            CollabRequest::OpenSelectedV2 { request_id, .. } => {
                if let Some(inbox) = inboxes.get_mut(&from) {
                    inbox.push_back(CollabEvent::OpenSelectedV2Result {
                        request_id,
                        result: Err("memory relay has no selected-file authority".into()),
                    });
                }
            }
            CollabRequest::OpenSelected { request_id, .. } => {
                if let Some(inbox) = inboxes.get_mut(&from) {
                    inbox.push_back(CollabEvent::OpenSelectedResult {
                        request_id,
                        result: Err("memory relay has no selected-file authority".into()),
                    });
                }
            }
            CollabRequest::LocalFileClipboard { request_id, .. } => {
                refuse_file_clipboard(&mut inboxes, &from, request_id);
            }
            CollabRequest::WorkspaceControlInput { .. }
            | CollabRequest::OfferContent { .. }
            | CollabRequest::WantContent { .. }
            | CollabRequest::CloseWorkspaceControl { .. }
            | CollabRequest::NearbyTransferCommand { .. }
            | CollabRequest::NativeMediaCommand { .. }
            | CollabRequest::RealtimeCommand { .. }
            | CollabRequest::Hello { .. }
            | CollabRequest::PublishActions { .. }
            | CollabRequest::PublishActionSurface { .. }
            | CollabRequest::ReportContinuation { .. }
            | CollabRequest::ReportPlayback { .. }
            | CollabRequest::ClearContinuation { .. }
            | CollabRequest::ReportHandoff { .. }
            | CollabRequest::CompleteInvocation { .. }
            | CollabRequest::UpdateInvocation { .. }
            | CollabRequest::RecordLocalEffect { .. }
            | CollabRequest::RemoteAction { .. }
            | CollabRequest::InvokePublishedAction { .. }
            | CollabRequest::QueryActionDirectory { .. }
            | CollabRequest::ResolvePublishedAction { .. }
            | CollabRequest::InvokeResolvedPublishedAction { .. }
            | CollabRequest::PublishedInvocationStatus { .. }
            | CollabRequest::CancelPublishedInvocation { .. }
            | CollabRequest::LaunchPublishedApplication { .. }
            | CollabRequest::LaunchInstalledApplication { .. }
            | CollabRequest::ReportTaskRun { .. }
            | CollabRequest::InvokeTaskAction { .. }
            | CollabRequest::ManagedInvocationAttempt { .. }
            | CollabRequest::TaskRunCancellation { .. } => {
                // Session-local and shell-owned control-plane commands never
                // fan out through another application's document channel.
            }
        }
    }
}

fn relay_mutation(
    mutation: tilo_ui::collaboration::DocumentMutation,
) -> collab_types::DocumentMutation {
    match mutation {
        tilo_ui::collaboration::DocumentMutation::Incremental(data) => {
            collab_types::DocumentMutation::Incremental(data)
        }
        tilo_ui::collaboration::DocumentMutation::Snapshot(data) => {
            collab_types::DocumentMutation::Snapshot(data)
        }
    }
}

fn respond_with_empty_scrollback(
    inboxes: &mut PeerInboxes,
    from: &str,
    artifact: collab_types::ArtifactId,
) {
    // The test broker holds no op log — answer the requester
    // with an empty, exhausted batch.
    if let Some(inbox) = inboxes.get_mut(from) {
        inbox.push_back(CollabEvent::ScrollbackBatch {
            artifact,
            ops: Vec::new(),
            decrypted_payloads: Vec::new(),
            oldest_seq: None,
            exhausted: true,
        });
    }
}

fn request_document_snapshot(inboxes: &mut PeerInboxes, peer_ids: &[String], from: &str) {
    // Production chooses the lowest broker peer id other than the
    // requester. This route-agnostic harness has one shared test
    // channel, so use the lexical peer id for the same one-holder
    // invariant.
    if let Some(id) = peer_ids.iter().filter(|id| id.as_str() != from).min()
        && let Some(inbox) = inboxes.get_mut(id)
    {
        inbox.push_back(CollabEvent::DocumentSnapshotRequested {
            route: tilo_ui::collaboration::RouteSpec::cluster(uuid::Uuid::nil()),
        });
    }
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

fn refuse_file_clipboard(inboxes: &mut PeerInboxes, from: &str, request_id: u64) {
    if let Some(inbox) = inboxes.get_mut(from) {
        inbox.push_back(CollabEvent::LocalFileClipboardResult {
            request_id,
            result: Err("memory relay has no launcher-attested Files service".into()),
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

fn document_event(mutation: &collab_types::DocumentMutation, from_user_id: &str) -> CollabEvent {
    let mut envelope = collab_types::SignedOp::new_unsigned(
        mutation.encode().expect("test document mutation encodes"),
        collab_types::ArtifactId::new(),
        collab_types::Ed25519PublicKey::from_bytes([0; 32]),
        collab_types::AttestationId::new(),
        collab_types::OpKind::Change,
        std::time::SystemTime::UNIX_EPOCH,
        0,
    );
    envelope.signature = collab_types::Signature::from_bytes([0; 64]);
    CollabEvent::Document {
        envelope: Box::new(envelope),
        from_user_id: from_user_id.to_owned(),
        decrypted_payload: None,
    }
}

pub fn document_mutation(event: &CollabEvent) -> Option<collab_types::DocumentMutation> {
    event.document_mutation()?.ok()
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
