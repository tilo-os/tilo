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
//! - Content a record references and this page lacks (`WantContent`, a
//!   picture's hash) is asked of the room on its presence, as every
//!   device asks; the host and any member keeping it answer this device
//!   alone, in pieces. Only pieces of content this page asked for are kept,
//!   and the assembled bytes reach the app (`CollabEvent::RecordContent`)
//!   only when they hash to what was asked for.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use tilo_relay_protocol::live::{
    AnchorScope, LiveAnchor, LiveFrame, read_selection_anchors, selection_anchors,
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

/// Largest content one reference names, as the room's hosts keep it.
const MAX_CONTENT_BYTES: u64 = 64 * 1024 * 1024;
/// Most bytes one piece carries.
const PIECE_BYTES: usize = 192 * 1024;
/// How long an unanswered request keeps collecting pieces, and how long
/// before the same content is asked of the room again.
const WANT_TTL_MS: u64 = 60_000;
/// Requests open at once; past it, the oldest goes.
const MAX_OPEN_WANTS: usize = 256;

/// Content this page asked a room for, and the pieces of it that came.
struct Want {
    route: RouteSpec,
    asked_ms: u64,
    total: Option<u64>,
    pieces: BTreeMap<u64, Vec<u8>>,
}

impl Want {
    /// The whole content once its pieces cover it, end to end.
    fn assembled(&self) -> Option<Vec<u8>> {
        let total = self.total?;
        let mut bytes = Vec::with_capacity(usize::try_from(total).ok()?);
        for (offset, piece) in &self.pieces {
            if *offset != u64::try_from(bytes.len()).ok()? {
                return None;
            }
            bytes.extend_from_slice(piece);
        }
        (u64::try_from(bytes.len()).ok()? == total).then_some(bytes)
    }
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
    wants: BTreeMap<String, Want>,
}

impl RelayCollabTransport {
    /// Wrap a running session.
    pub fn new(peer: PeerHandle) -> Self {
        Self {
            inbound: peer.events(),
            peer,
            live: crate::live::LiveState::new(tilo_clock::unix_now_ms()),
            primary: None,
            confirm_pending: false,
            primary_results: VecDeque::new(),
            routes: BTreeSet::new(),
            ready: BTreeSet::new(),
            events: VecDeque::new(),
            wants: BTreeMap::new(),
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
            CollabRequest::WantContent { hash } => self.want_content(route, &hash),
            _ => Err(TransportError::Unsupported),
        }
    }

    /// This page's handle in content requests and answers.
    fn own_handle(&self) -> neighbour_wire::PeerId {
        neighbour_wire::peer_handle(&self.peer.status().device)
    }

    /// Ask the room for content `route`'s record references, unless it was
    /// asked for within [`WANT_TTL_MS`].
    fn want_content(&mut self, route: &RouteSpec, hash: &str) -> Result<(), TransportError> {
        let workspace = workspace(route).ok_or(TransportError::Unsupported)?;
        let hash = hash.to_ascii_lowercase();
        let now = tilo_clock::unix_now_ms();
        if self
            .wants
            .get(&hash)
            .is_some_and(|want| now.saturating_sub(want.asked_ms) < WANT_TTL_MS)
        {
            return Ok(());
        }
        if self.wants.len() >= MAX_OPEN_WANTS
            && !self.wants.contains_key(&hash)
            && let Some(oldest) = self
                .wants
                .iter()
                .min_by_key(|(_, want)| want.asked_ms)
                .map(|(hash, _)| hash.clone())
        {
            self.wants.remove(&oldest);
        }
        let request = neighbour_wire::NeighbourMsg::RecordContentWant {
            requester: self.own_handle(),
            app_id: route.app_id.clone(),
            cluster_id: route.cluster_id.to_string(),
            hash: hash.clone(),
        };
        let request = postcard::to_allocvec(&request).map_err(|_| TransportError::Unsupported)?;
        self.wants.insert(
            hash,
            Want {
                route: route.clone(),
                asked_ms: now,
                total: None,
                pieces: BTreeMap::new(),
            },
        );
        self.peer
            .want_content(workspace, request)
            .map_err(|_| TransportError::Closed)
    }

    /// One piece of content a member sent this page: kept when it belongs to
    /// content this page asked for, and the content handed on once whole and
    /// verified.
    fn content_piece(&mut self, frame: &[u8]) {
        let Ok(neighbour_wire::NeighbourMsg::RecordContentPiece {
            requester,
            app_id,
            cluster_id,
            hash,
            total,
            offset,
            bytes,
            ..
        }) = postcard::from_bytes(frame)
        else {
            return;
        };
        let hash = hash.to_ascii_lowercase();
        if requester != self.own_handle() || total > MAX_CONTENT_BYTES || bytes.len() > PIECE_BYTES
        {
            return;
        }
        let Some(want) = self.wants.get_mut(&hash) else {
            return;
        };
        if want.route.app_id != app_id || want.route.cluster_id.to_string() != cluster_id {
            return;
        }
        if want.total.is_some_and(|known| known != total) {
            return;
        }
        let fits = u64::try_from(bytes.len())
            .ok()
            .and_then(|len| offset.checked_add(len))
            .is_some_and(|end| end <= total);
        if !fits {
            return;
        }
        want.total = Some(total);
        want.pieces.entry(offset).or_insert(bytes);
        let Some(assembled) = want.assembled() else {
            return;
        };
        let Some(want) = self.wants.remove(&hash) else {
            return;
        };
        if blake3::hash(&assembled).to_hex().as_str() == hash {
            self.events.push_back(CollabEvent::RecordContent {
                route: want.route,
                hash,
                bytes: assembled,
            });
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
            .publish_presence(tilo_clock::unix_now_ms(), |presence| {
                presence.focus.clone_from(&focus);
                match payload {
                    PresencePayload::Pointer { x, y } => presence.pointer = Some([*x, *y]),
                    PresencePayload::Caret { pos } => {
                        presence.selection = presence_anchors(key, *pos, *pos);
                    }
                    PresencePayload::Selection { start, end } => {
                        presence.selection = presence_anchors(key, *start, *end);
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
                PeerEvent::RecordContent { frame, .. } => self.content_piece(&frame),
                PeerEvent::LiveMember { device, active, .. } => {
                    self.live.member(device, active);
                }
                // Remote actions and voice are the page's (`agents.mjs`,
                // `actionsV1`, `voice-peer.mjs`), consumed from their own queues. The toolkit keeps its
                // documents in the collaboration manager, so another
                // volunteer answers compaction requests. A refused commit is
                // the record owner's to withdraw; this transport holds none.
                PeerEvent::DocumentRejected(_)
                | PeerEvent::Committed { .. }
                | PeerEvent::CommitRefused { .. }
                | PeerEvent::RemoteAction { .. }
                | PeerEvent::RemoteActionUndeliverable { .. }
                | PeerEvent::CompactionRequested { .. }
                | PeerEvent::RecordOpened { .. }
                | PeerEvent::Voice(_) => {}
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
            .receive(from, frame, tilo_clock::unix_now_ms())
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
            .and_then(|(anchor, head)| read_presence_anchors(anchor, head))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn want(total: u64, pieces: &[(u64, &[u8])]) -> Want {
        Want {
            route: RouteSpec {
                app_id: "so.tilo.chat".into(),
                cluster_id: Default::default(),
                record: None,
            },
            asked_ms: 0,
            total: Some(total),
            pieces: pieces
                .iter()
                .map(|(at, bytes)| (*at, bytes.to_vec()))
                .collect(),
        }
    }

    #[test]
    fn content_is_whole_once_its_pieces_cover_it_in_any_order() {
        assert_eq!(
            want(6, &[(3, b"def"), (0, b"abc")]).assembled().as_deref(),
            Some(&b"abcdef"[..])
        );
        assert_eq!(want(0, &[(0, b"")]).assembled().as_deref(), Some(&b""[..]));
    }

    #[test]
    fn content_with_a_gap_or_short_of_its_total_is_not_whole() {
        assert!(want(9, &[(0, b"abc"), (6, b"ghi")]).assembled().is_none());
        assert!(want(9, &[(0, b"abc"), (3, b"def")]).assembled().is_none());
        assert!(
            Want {
                total: None,
                ..want(3, &[(0, b"abc")])
            }
            .assembled()
            .is_none()
        );
    }
}
