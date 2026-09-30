//! The certified relay session as the toolkit's [`CollabTransport`], for
//! `CollaborationManager::with_transport`.
//!
//! - A primary `Hello` joins the route's room (and its live plane); the
//!   primary route is acknowledged once the host answers `Ready`.
//! - Document mutations are signed and committed by the session
//!   ([`PeerHandle::publish`]); inbound documents arrive already verified as
//!   [`CollabEvent::Document`]. The publication barrier completes when the
//!   host has made every earlier change durable.
//! - Presence rides the live plane: pointers, carets and selections become
//!   [`PresenceFrame`]s whose selection anchors carry the editor's
//!   [`PresenceKey`] (`[1, uvarint(start), postcard(key)]` / `[2,
//!   uvarint(end)]`, as in PR 541's `live_view`), and remote frames come back
//!   as `RemoteCursorMoved` / `RemotePresenceUpdate`.
//! - `InstallDocumentEncryption` seals and opens the route with its
//!   `channel-crypto` group.

use std::collections::{BTreeSet, VecDeque};
use std::sync::Arc;

use tilo_relay_protocol::live::{
    ANCHOR_TAG_SAME_SCOPE, ANCHOR_TAG_SCOPED, LiveAnchor, LiveFrame, MAX_OBJECT_ID_BYTES,
};
use ui::collaboration::{
    ActionDirectoryRequest, CollabEvent, CollabRequest, CollabTransport, DocumentPublicationError,
    DocumentPublicationReceipt, PresencePayload, RouteSpec, TransportCommand, TransportError,
    TransportStatus,
};
use ui::presence::PresenceKey;

use crate::document::{ContentCipher, DocumentRoute};
use crate::session::{PeerEvent, PeerHandle, TransportPhase};

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

fn push_uvarint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = u8::try_from(value & 0x7f).unwrap_or(0);
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn read_uvarint(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0u64;
    for (index, (byte, shift)) in bytes.iter().zip((0u32..64).step_by(7)).enumerate() {
        value |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some((value, bytes.get(index.checked_add(1)?..)?));
        }
    }
    None
}

/// A caret (`start == end`) or selection in the editor `key` as the live
/// plane's selection anchors; `None` when the key does not fit an anchor.
pub fn presence_anchors(
    key: &PresenceKey,
    start: usize,
    end: usize,
) -> Option<(LiveAnchor, LiveAnchor)> {
    anchors(key, start, end)
}

/// The editor key and range of selection anchors made by
/// [`presence_anchors`].
pub fn read_presence_anchors(
    anchor: &LiveAnchor,
    head: &LiveAnchor,
) -> Option<(PresenceKey, usize, usize)> {
    read_anchors(anchor, head)
}

fn anchors(key: &PresenceKey, start: usize, end: usize) -> Option<(LiveAnchor, LiveAnchor)> {
    let mut anchor = vec![ANCHOR_TAG_SCOPED];
    push_uvarint(&mut anchor, u64::try_from(start).ok()?);
    anchor.extend(postcard::to_stdvec(key).ok()?);
    let mut head = vec![ANCHOR_TAG_SAME_SCOPE];
    push_uvarint(&mut head, u64::try_from(end).ok()?);
    (anchor.len() <= MAX_OBJECT_ID_BYTES)
        .then_some((LiveAnchor { cursor: anchor }, LiveAnchor { cursor: head }))
}

fn read_anchors(anchor: &LiveAnchor, head: &LiveAnchor) -> Option<(PresenceKey, usize, usize)> {
    let (&tag, rest) = anchor.cursor.split_first()?;
    if tag != ANCHOR_TAG_SCOPED {
        return None;
    }
    let (start, key) = read_uvarint(rest)?;
    let key: PresenceKey = postcard::from_bytes(key).ok()?;
    let (&tag, rest) = head.cursor.split_first()?;
    if tag != ANCHOR_TAG_SAME_SCOPE {
        return None;
    }
    let (end, _) = read_uvarint(rest)?;
    Some((
        key,
        usize::try_from(start).ok()?,
        usize::try_from(end).ok()?,
    ))
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
}

impl RelayCollabTransport {
    /// Wrap a running session.
    pub fn new(peer: PeerHandle) -> Self {
        Self {
            inbound: peer.events(),
            peer,
            live: crate::live::LiveState::new(crate::clock::now_ms()),
            primary: None,
            confirm_pending: false,
            primary_results: VecDeque::new(),
            routes: BTreeSet::new(),
            ready: BTreeSet::new(),
            events: VecDeque::new(),
        }
    }

    fn join(&mut self, route: &RouteSpec) -> Result<u64, TransportError> {
        let workspace = workspace(route).ok_or(TransportError::Unsupported)?;
        self.routes.insert(document_route(route));
        self.peer
            .join(workspace)
            .and_then(|()| self.peer.live_join(workspace))
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
                self.peer
                    .publish(document_route(route), &mutation)
                    .map_err(|_| TransportError::Closed)
            }
            CollabRequest::Presence { key, payload } => self.presence(route, &key, &payload),
            CollabRequest::RequestDocumentSnapshot => Ok(()),
            _ => Err(TransportError::Unsupported),
        }
    }

    fn presence(
        &mut self,
        route: &RouteSpec,
        key: &PresenceKey,
        payload: &PresencePayload,
    ) -> Result<(), TransportError> {
        let workspace = workspace(route).ok_or(TransportError::Shed)?;
        let focus = route
            .record
            .as_ref()
            .map(|record| tilo_relay_protocol::live::LiveRecord {
                app: route.app_id.clone(),
                record: record.clone(),
            });
        let frame = self
            .live
            .publish_presence(crate::clock::now_ms(), |presence| {
                presence.focus.clone_from(&focus);
                match payload {
                    PresencePayload::Pointer { x, y } => presence.pointer = Some([*x, *y]),
                    PresencePayload::Caret { pos } => {
                        presence.selection = anchors(key, *pos, *pos);
                    }
                    PresencePayload::Selection { start, end } => {
                        presence.selection = anchors(key, *start, *end);
                    }
                    PresencePayload::Left => {
                        presence.pointer = None;
                        presence.selection = None;
                    }
                    _ => {}
                }
            })
            .map_err(|_| TransportError::Shed)?;
        self.peer
            .publish_live(workspace, frame)
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
                PeerEvent::LiveMember { device, active, .. } => {
                    self.live.member(device, active);
                }
                // Remote actions are the page's (`agents.mjs`, `actionsV1`),
                // consumed from their own event queue. The toolkit keeps its
                // documents in the collaboration manager, so another
                // volunteer answers compaction requests.
                PeerEvent::DocumentRejected(_)
                | PeerEvent::Committed { .. }
                | PeerEvent::RemoteAction { .. }
                | PeerEvent::RemoteActionUndeliverable { .. }
                | PeerEvent::CompactionRequested { .. } => {}
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

    fn remote_presence(&mut self, from: [u8; 32], frame: &[u8]) {
        let Ok(LiveFrame::Presence(presence)) = LiveFrame::decode(frame) else {
            return;
        };
        if self
            .live
            .receive(from, frame, crate::clock::now_ms())
            .is_none()
        {
            return;
        }
        let user_id = hex::encode(from);
        if let Some([x, y]) = presence.pointer {
            self.events.push_back(CollabEvent::RemoteCursorMoved {
                user_id: user_id.clone(),
                x,
                y,
            });
        }
        if let Some((key, start, end)) = presence
            .selection
            .as_ref()
            .and_then(|(anchor, head)| read_anchors(anchor, head))
        {
            let payload = if start == end {
                PresencePayload::Caret { pos: start }
            } else {
                PresencePayload::Selection { start, end }
            };
            self.events.push_back(CollabEvent::RemotePresenceUpdate {
                user_id,
                key,
                payload,
            });
        }
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
                TransportStatus::Closed("the host no longer accepts this browser".into())
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
            TransportCommand::Join(route) => self.join(&route).map(|_| ()),
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
                let _ = reply.send(result.map_err(DocumentPublicationError::Failed));
            }))
            .map_err(|_| DocumentPublicationError::Unavailable)?;
        Ok(DocumentPublicationReceipt::new(receipt))
    }

    fn set_wake(&mut self, wake: Box<dyn Fn() + Send + Sync>) {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::from(wake);
        self.peer.on_change(wake);
    }
}
