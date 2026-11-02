//! The certified relay session as the toolkit's [`CollabTransport`], for
//! `CollaborationManager::with_transport`.
//!
//! - A primary `Hello` joins the route's workspace (and its live plane); the
//!   primary route is acknowledged once the host answers `Ready`.
//! - Document mutations are signed and committed by the session
//!   ([`PeerHandle::publish`]); inbound documents arrive already verified as
//!   [`CollabEvent::Document`]. The publication barrier completes when the
//!   host has made every earlier change durable.
//! - Presence rides the workspace's presence, as every device's does: a
//!   pointer, caret, selection, typing range, status or wave under the
//!   editor's [`PresenceKey`] goes out as an app-presence frame, and the
//!   members' frames come back as `RemoteCursorMoved` /
//!   `RemotePresenceUpdate`.
//! - `InstallDocumentEncryption` seals and opens the route with its
//!   `channel-crypto` group.
//! - Content a record references and this page lacks (`WantContent`, a
//!   picture's hash) is asked of the workspace on its presence, as every
//!   device asks; the host and any member keeping it answer this device
//!   alone, in pieces. Only pieces of content this page asked for are kept,
//!   and the assembled bytes reach the app (`CollabEvent::RecordContent`)
//!   only when they hash to what was asked for.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use collab_types::PeerPresencePayload;
use tilo_relay_protocol::live::{
    AnchorScope, LiveAnchor, read_selection_anchors, selection_anchors,
};
use ui::collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, CollabTransport, DocumentPublicationError,
    DocumentPublicationReceipt, PresencePayload, RouteSpec, TransportCommand, TransportError,
    TransportStatus,
};
use ui::presence::PresenceKey;

use crate::session::{PeerEvent, PeerHandle, TransportPhase};
use tilo_peer_state::document_frame::{ContentCipher, DocumentRoute};

/// A `channel-crypto` member group as a [`ContentCipher`].
struct GroupCipher(Box<channel_crypto::ChannelGroup>);

impl ContentCipher for GroupCipher {
    fn seal(
        &mut self,
        artifact: collab_types::ArtifactId,
        retention: collab_types::ContentRetention,
        cleartext: &[u8],
    ) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> {
        self.0
            .seal_content(artifact, retention, cleartext)
            .map_err(|error| error.to_string())
    }

    fn open(
        &mut self,
        artifact: collab_types::ArtifactId,
        metadata: &collab_types::EncryptionMetadata,
        payload: &[u8],
    ) -> Result<Vec<u8>, String> {
        self.0
            .open_content(artifact, metadata, payload)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }
}

fn document_route(route: &RouteSpec) -> DocumentRoute {
    DocumentRoute {
        app_id: route.app_id.clone(),
        cluster_id: route.cluster_id.to_string(),
        record: route.record.clone().unwrap_or_default(),
    }
}

fn workspace(route: &RouteSpec) -> Option<u64> {
    tilo_relay_protocol::room_workspace_id(&route.cluster_id.to_string()).ok()
}

/// A caret (`start == end`) or selection in the editor `key` as the live
/// plane's selection anchors. A key too long for the anchor (a framework
/// resource key naming a long item id) travels as its scope digest, which the
/// receiving editor resolves against its own keys
/// ([`PresenceKey::digest_form`]); `None` when even that does not fit.
pub fn presence_anchors(
    key: &PresenceKey,
    start: usize,
    end: usize,
) -> Option<(LiveAnchor, LiveAnchor)> {
    selection_anchors(
        &postcard::to_stdvec(key).ok()?,
        || key.scope_digest(),
        u64::try_from(start).ok()?,
        u64::try_from(end).ok()?,
    )
}

/// The editor key and range of selection anchors made by
/// [`presence_anchors`].
pub fn read_presence_anchors(
    anchor: &LiveAnchor,
    head: &LiveAnchor,
) -> Option<(PresenceKey, usize, usize)> {
    let (scope, start, end) = read_selection_anchors(anchor, head)?;
    let key = match scope {
        AnchorScope::Key(key) => postcard::from_bytes(key).ok()?,
        AnchorScope::Digest(digest) => PresenceKey::from_scope_digest(digest),
    };
    Some((
        key,
        usize::try_from(start).ok()?,
        usize::try_from(end).ok()?,
    ))
}

/// `payload` under `key` as the workspace's presence carries an app's
/// presence: cluster-scoped, and authored by the device that publishes it.
/// `None` for a payload this device cannot say.
fn app_presence(
    device: [u8; 32],
    cluster_id: &str,
    app_id: &str,
    key: &PresenceKey,
    payload: &PresencePayload,
) -> Option<Vec<u8>> {
    let payload = match payload {
        PresencePayload::Pointer { x, y } => PeerPresencePayload::Pointer { x: *x, y: *y },
        PresencePayload::Caret { pos } => PeerPresencePayload::Caret {
            pos: u64::try_from(*pos).ok()?,
        },
        PresencePayload::Selection { start, end } => PeerPresencePayload::Selection {
            start: u64::try_from(*start).ok()?,
            end: u64::try_from(*end).ok()?,
        },
        PresencePayload::Typing { start, end } => PeerPresencePayload::Typing {
            start: u64::try_from(*start).ok()?,
            end: u64::try_from(*end).ok()?,
        },
        PresencePayload::Arrived => PeerPresencePayload::Arrived,
        PresencePayload::Left => PeerPresencePayload::Left,
        PresencePayload::Following { target } => PeerPresencePayload::Following {
            target: target.clone(),
        },
        PresencePayload::Interaction(interaction) => {
            PeerPresencePayload::InteractionJson(serde_json::to_string(&**interaction).ok()?)
        }
        PresencePayload::Status { text, automatic } => PeerPresencePayload::Status {
            text: text.clone(),
            automatic: *automatic,
        },
        PresencePayload::Availability { state, call } => PeerPresencePayload::Availability {
            state: state.id().to_owned(),
            call: call.clone(),
            camera: false,
            screen: false,
        },
        PresencePayload::Composing { composing } => PeerPresencePayload::Composing {
            composing: *composing,
        },
    };
    let mut peer = [0; 8];
    peer.copy_from_slice(device.get(..8)?);
    let message = neighbour_wire::NeighbourMsg::AppPresenceV2 {
        peer: u64::from_le_bytes(peer),
        app_id: app_id.to_owned(),
        cluster_id: cluster_id.to_owned(),
        presence: collab_types::PeerPresence {
            key: postcard::to_stdvec(key).ok()?,
            payload,
        },
        colour: None,
    };
    postcard::to_stdvec(&message).ok()
}

/// The certified relay session as a [`CollabTransport`].
pub struct RelayCollabTransport {
    peer: PeerHandle,
    inbound: crate::session::PeerEvents,
    live: crate::live::LiveState,
    primary: Option<RouteSpec>,
    confirm_pending: bool,
    primary_results: VecDeque<Result<RouteSpec, String>>,
    routes: BTreeSet<DocumentRoute>,
    ready: BTreeSet<u64>,
    events: VecDeque<CollabEvent>,
    wants: crate::content::ContentWants<RouteSpec>,
}

impl RelayCollabTransport {
    /// Wrap a running session.
    pub fn new(peer: PeerHandle) -> Self {
        Self {
            inbound: peer.events(),
            peer,
            live: crate::live::LiveState::starting_now(),
            primary: None,
            confirm_pending: false,
            primary_results: VecDeque::new(),
            routes: BTreeSet::new(),
            ready: BTreeSet::new(),
            events: VecDeque::new(),
            wants: crate::content::ContentWants::default(),
        }
    }

    fn join(&mut self, route: &RouteSpec) -> Result<u64, TransportError> {
        let workspace = workspace(route).ok_or(TransportError::Unsupported)?;
        self.routes.insert(document_route(route));
        self.peer
            .join(workspace)
            .and_then(|()| self.peer.open_route(document_route(route)))
            .map_err(|_| TransportError::Closed)?;
        Ok(workspace)
    }

    fn publish(&mut self, route: &RouteSpec, request: CollabRequest) -> Result<(), TransportError> {
        match request {
            CollabRequest::Document { mutation, .. } => {
                let size = mutation.data().len();
                if size > collab_types::MAX_DOCUMENT_TRANSFER_BYTES {
                    return Err(TransportError::TooLarge {
                        actual: size,
                        max: collab_types::MAX_DOCUMENT_TRANSFER_BYTES,
                    });
                }
                // A refusal comes back through the publication barrier as
                // the host's answer, not as a lost session: `publish` queues
                // the change and returns here, and the manager tells the two
                // apart by what the barrier reports.
                self.peer
                    .publish(document_route(route), &mutation)
                    .map_err(|_| TransportError::Closed)
            }
            CollabRequest::Presence { key, payload } => self.presence(route, &key, &payload),
            CollabRequest::RequestDocumentSnapshot => Ok(()),
            CollabRequest::WantContent { hash } => self.want_content(route, &hash),
            _ => Err(TransportError::Unsupported),
        }
    }

    /// Ask the workspace for content `route`'s record references, unless it was
    /// asked for lately ([`crate::content::ContentWants::ask`]).
    fn want_content(&mut self, route: &RouteSpec, hash: &str) -> Result<(), TransportError> {
        let workspace = workspace(route).ok_or(TransportError::Unsupported)?;
        let device = self.peer.status().device;
        let cluster_id = route.cluster_id.to_string();
        let Some(request) = self.wants.ask(
            &device,
            route.clone(),
            (&route.app_id, &cluster_id),
            hash,
            tilo_clock::unix_now_ms(),
        ) else {
            return Ok(());
        };
        self.peer
            .want_content(workspace, request)
            .map_err(|_| TransportError::Closed)
    }

    /// One piece of content a member sent this page; the content reaches
    /// the app once whole and verified.
    fn content_piece(&mut self, frame: &[u8]) {
        let device = self.peer.status().device;
        if let Some(arrived) = self.wants.piece(&device, frame) {
            self.events.push_back(CollabEvent::RecordContent {
                route: arrived.tag,
                hash: arrived.hash,
                bytes: arrived.bytes,
            });
        }
    }

    /// `payload` under `key` for the route's workspace: what this device's
    /// app says about where its person is, in the app-presence frame every
    /// device of the workspace publishes.
    fn presence(
        &self,
        route: &RouteSpec,
        key: &PresenceKey,
        payload: &PresencePayload,
    ) -> Result<(), TransportError> {
        let workspace = workspace(route).ok_or(TransportError::Shed)?;
        let body = app_presence(
            self.peer.status().device,
            &route.cluster_id.to_string(),
            &route.app_id,
            key,
            payload,
        )
        .ok_or(TransportError::Shed)?;
        self.peer
            .publish_presence(workspace, vec![body])
            .map_err(|_| TransportError::Shed)
    }

    fn pump(&mut self) {
        for event in self.inbound.drain() {
            match event {
                PeerEvent::Ready { workspace_id } => {
                    self.ready.insert(workspace_id);
                }
                PeerEvent::Document(document) => {
                    // The toolkit takes whole frames only; a reassembled
                    // transfer's envelope is just its last segment.
                    if !self.routes.contains(&document.route)
                        || document.is_reassembled()
                        || document.mutation().is_none()
                    {
                        continue;
                    }
                    self.live
                        .durable_changed(&tilo_relay_protocol::live::LiveRecord {
                            app: document.route.app_id.clone(),
                            record: document.route.record.clone(),
                        });
                    self.events.push_back(CollabEvent::Document {
                        from_user_id: hex::encode(document.author),
                        envelope: Box::new(document.envelope),
                        decrypted_payload: document.decrypted_payload,
                    });
                }
                PeerEvent::Live { from, frame, .. } => self.remote_presence(from, &frame),
                PeerEvent::Presence { from, body, .. } => self.remote_app_presence(from, &body),
                PeerEvent::RecordContent { frame, .. } => self.content_piece(&frame),
                PeerEvent::LiveMember { device, active, .. } => {
                    self.live.member(device, active);
                }
                // Remote actions and voice are the page's (`agents.mjs`,
                // `actionsV1`, `voice-peer.mjs`), consumed from their own queues. The toolkit keeps its
                // documents in the collaboration manager, so another
                // volunteer answers compaction requests. A refused commit is
                // the record owner's to withdraw; this transport holds none,
                // nor content to answer a member's request with.
                PeerEvent::DocumentRejected(_)
                | PeerEvent::Committed { .. }
                | PeerEvent::CommitRefused { .. }
                | PeerEvent::RemoteAction { .. }
                | PeerEvent::RemoteActionUndeliverable { .. }
                | PeerEvent::CompactionRequested { .. }
                | PeerEvent::RecordOpened { .. }
                | PeerEvent::ContentWanted { .. }
                | PeerEvent::Voice(_)
                | PeerEvent::RoomLayout { .. } => {}
            }
        }
        if self.confirm_pending
            && let Some(primary) = &self.primary
            && workspace(primary).is_some_and(|workspace| self.ready.contains(&workspace))
        {
            self.confirm_pending = false;
            self.primary_results.push_back(Ok(primary.clone()));
        }
    }

    /// A member's app presence: its pointer, caret, selection or typing range
    /// under an editor key this transport's app gave it.
    fn remote_app_presence(&mut self, from: [u8; 32], body: &[u8]) {
        let Ok(neighbour_wire::NeighbourMsg::AppPresenceV2 {
            app_id,
            cluster_id,
            presence,
            ..
        }) = postcard::from_bytes(body)
        else {
            return;
        };
        if !self
            .routes
            .iter()
            .any(|route| route.app_id == app_id && route.cluster_id == cluster_id)
        {
            return;
        }
        let Ok(key) = postcard::from_bytes::<PresenceKey>(&presence.key) else {
            return;
        };
        let user_id = hex::encode(from);
        match presence.payload {
            PeerPresencePayload::Pointer { x, y } => {
                self.events
                    .push_back(CollabEvent::RemoteCursorMoved { user_id, x, y });
            }
            PeerPresencePayload::Caret { pos } => {
                self.events.push_back(CollabEvent::RemotePresenceUpdate {
                    user_id,
                    key,
                    payload: PresencePayload::Caret {
                        pos: usize::try_from(pos).unwrap_or(usize::MAX),
                    },
                });
            }
            PeerPresencePayload::Selection { start, end } => {
                self.events.push_back(CollabEvent::RemotePresenceUpdate {
                    user_id,
                    key,
                    payload: PresencePayload::Selection {
                        start: usize::try_from(start).unwrap_or(usize::MAX),
                        end: usize::try_from(end).unwrap_or(usize::MAX),
                    },
                });
            }
            PeerPresencePayload::Typing { start, end } => {
                self.events.push_back(CollabEvent::RemotePresenceUpdate {
                    user_id,
                    key,
                    payload: PresencePayload::Typing {
                        start: usize::try_from(start).unwrap_or(usize::MAX),
                        end: usize::try_from(end).unwrap_or(usize::MAX),
                    },
                });
            }
            PeerPresencePayload::Arrived
            | PeerPresencePayload::Left
            | PeerPresencePayload::Following { .. }
            | PeerPresencePayload::InteractionJson(_)
            | PeerPresencePayload::Status { .. }
            | PeerPresencePayload::Availability { .. }
            | PeerPresencePayload::Chat { .. }
            | PeerPresencePayload::Reaction { .. }
            | PeerPresencePayload::Wave { .. }
            | PeerPresencePayload::Composing { .. }
            | PeerPresencePayload::Shell(_) => {}
        }
    }

    /// The live plane's own frames are previews; presence is the workspace's.
    fn remote_presence(&mut self, from: [u8; 32], frame: &[u8]) {
        let _ = self.live.receive(from, frame, tilo_clock::unix_now_ms());
    }
}

impl CollabTransport for RelayCollabTransport {
    fn status(&self) -> TransportStatus {
        match self.peer.status().phase {
            TransportPhase::Connected => TransportStatus::Ready,
            TransportPhase::Connecting => TransportStatus::Connecting,
            TransportPhase::Reconnecting | TransportPhase::Offline => {
                TransportStatus::Degraded("reconnecting to the host; edits are queued".into())
            }
            TransportPhase::Failed => {
                TransportStatus::Closed("the workspace’s host did not let this browser in".into())
            }
            TransportPhase::Closed => TransportStatus::Closed("peer session closed".into()),
        }
    }

    fn submit(&mut self, command: TransportCommand) -> Result<(), TransportError> {
        match command {
            TransportCommand::Primary(CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                ..
            }) => {
                let route = RouteSpec {
                    app_id,
                    cluster_id,
                    record,
                };
                self.join(&route)?;
                self.primary = Some(route);
                Ok(())
            }
            TransportCommand::Primary(request) => {
                let route = self.primary.clone().ok_or(TransportError::Closed)?;
                self.publish(&route, request)
            }
            TransportCommand::ToRoute(route, request) => {
                self.join(&route)?;
                self.publish(&route, request)
            }
            TransportCommand::Join(route, _) => self.join(&route).map(|_| ()),
            TransportCommand::ConfirmPrimaryRoute => {
                self.confirm_pending = true;
                Ok(())
            }
            TransportCommand::InstallDocumentEncryption {
                route,
                group,
                reply,
            } => {
                let result = self
                    .peer
                    .install_cipher(document_route(&route), Box::new(GroupCipher(group)));
                let _ = reply.send(result.clone());
                result.map_err(|_| TransportError::Closed)
            }
            TransportCommand::PublishActionDirectory { .. } => Err(TransportError::Unsupported),
        }
    }

    fn drain(&mut self, out: &mut Vec<CollabEvent>) {
        self.pump();
        out.extend(self.events.drain(..));
    }

    fn take_primary_route_result(&mut self) -> Option<Result<RouteSpec, String>> {
        self.pump();
        self.primary_results.pop_front()
    }

    fn take_action_directory_requests(&mut self, _out: &mut Vec<ActionDirectoryRequest>) {}

    fn publication_barrier(
        &mut self,
    ) -> Result<DocumentPublicationReceipt, DocumentPublicationError> {
        let (reply, receipt) = std::sync::mpsc::sync_channel(1);
        self.peer
            .barrier_then(Box::new(move |result| {
                // The host answered "no" about the change itself. That is not
                // a lost session: the manager keeps the app connected and
                // fails only that change.
                let _ = reply.send(result.map_err(|reason| {
                    if crate::session::is_refusal(&reason) {
                        DocumentPublicationError::Refused(reason)
                    } else {
                        DocumentPublicationError::Failed(reason)
                    }
                }));
            }))
            .map_err(|_| DocumentPublicationError::Unavailable)?;
        Ok(DocumentPublicationReceipt::new(receipt))
    }

    fn set_wake(&mut self, wake: Box<dyn Fn() + Send + Sync>) {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::from(wake);
        self.peer.on_change(wake);
    }
}
