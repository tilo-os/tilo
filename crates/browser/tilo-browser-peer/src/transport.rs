//! The certified relay session as the toolkit's [`CollabTransport`], for
//! `CollaborationManager::with_transport`.
//!
//! - A primary `Hello` joins the route's workspace (and its live plane); the
//!   primary route is acknowledged once the host answers `Ready`.
//! - Document mutations are signed and committed by the session
//!   ([`PeerHandle::publish`]); inbound documents arrive already verified as
//!   [`CollabEvent::Document`]. The publication barrier completes when the
//!   host has made every earlier change durable.
//! - Presence rides the workspace's presence track, as every device's does: a
//!   pointer, caret, selection, typing range, status or wave under the
//!   editor's [`PresenceKey`] goes out as an app's [`Presence`] frame, and
//!   the members' frames come back from the plane as `RemoteCursorMoved` /
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
use collab_types::presence::Presence;
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
/// presence: the app of the route, in its workspace, authored by the device
/// that publishes it. `None` for a payload this device cannot say.
fn app_presence(
    cluster_id: &str,
    app_id: &str,
    key: &PresenceKey,
    payload: &PresencePayload,
) -> Option<Presence> {
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
    Some(Presence::app(app_id, cluster_id, key.clone(), payload))
}

/// The certified relay session as a [`CollabTransport`].
pub struct RelayCollabTransport {
    peer: PeerHandle,
    inbound: crate::session::PeerEvents,
    live: crate::previews::LiveState,
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
            live: crate::previews::LiveState::starting_now(),
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
        let body = app_presence(&route.cluster_id.to_string(), &route.app_id, key, payload)
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
                PeerEvent::RecordContent { frame, .. } => self.content_piece(&frame),
                // One member's frame on the workspace's live plane: the app
                // presence track is where an editor's cursors arrive, and a
                // record's preview frames were opened into [`PeerEvent::Live`]
                // before they reached here. Remote actions and voice are the
                // page's (`agents.mjs`, `actionsV1`, `voice-peer.mjs`), consumed
                // from their own queues. The toolkit keeps its
                // documents in the collaboration manager, so another
                // volunteer answers compaction requests. A refused commit is
                // the record owner's to withdraw; this transport holds none,
                // nor content to answer a member's request with.
                PeerEvent::LivePlane {
                    from,
                    track: tilo_live::live::Track::Presence,
                    body,
                    ..
                } => self.remote_app_presence(from, &body),
                PeerEvent::DocumentRejected(_)
                | PeerEvent::Committed { .. }
                | PeerEvent::CommitRefused { .. }
                | PeerEvent::RemoteAction { .. }
                | PeerEvent::RemoteActionUndeliverable { .. }
                | PeerEvent::LivePlane { .. }
                | PeerEvent::CompactionRequested { .. }
                | PeerEvent::RecordOpened { .. }
                | PeerEvent::ContentWanted { .. }
                | PeerEvent::Voice(_)
                | PeerEvent::CallAudio { .. }
                | PeerEvent::CallPicture { .. }
                | PeerEvent::CallDemand { .. } => {}
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
        let Some(said) = Presence::decode(body) else {
            return;
        };
        if let Some(event) = editor_presence(&self.routes, from, said) {
            self.events.push_back(event);
        }
    }

    /// The live plane's own frames are previews; presence is the workspace's.
    fn remote_presence(&mut self, from: [u8; 32], frame: &[u8]) {
        let _ = self.live.receive(from, frame, tilo_clock::unix_now_ms());
    }
}

/// The toolkit event a member's app presence is, when it is about an editor
/// this transport's app holds ([`DocumentRoute`]s of `routes`): its pointer,
/// caret, selection or typing range under the frame's own key. `None` for
/// another app's or workspace's frame, and for what an editor draws nothing
/// from.
fn editor_presence(
    routes: &BTreeSet<DocumentRoute>,
    from: [u8; 32],
    said: Presence,
) -> Option<CollabEvent> {
    if !routes
        .iter()
        .any(|route| route.app_id == said.app && route.cluster_id == said.cluster)
    {
        return None;
    }
    let key = said.key;
    let user_id = hex::encode(from);
    match said.payload {
        PeerPresencePayload::Pointer { x, y } => {
            Some(CollabEvent::RemoteCursorMoved { user_id, x, y })
        }
        PeerPresencePayload::Caret { pos } => Some(CollabEvent::RemotePresenceUpdate {
            user_id,
            key,
            payload: PresencePayload::Caret {
                pos: usize::try_from(pos).unwrap_or(usize::MAX),
            },
        }),
        PeerPresencePayload::Selection { start, end } => Some(CollabEvent::RemotePresenceUpdate {
            user_id,
            key,
            payload: PresencePayload::Selection {
                start: usize::try_from(start).unwrap_or(usize::MAX),
                end: usize::try_from(end).unwrap_or(usize::MAX),
            },
        }),
        PeerPresencePayload::Typing { start, end } => Some(CollabEvent::RemotePresenceUpdate {
            user_id,
            key,
            payload: PresencePayload::Typing {
                start: usize::try_from(start).unwrap_or(usize::MAX),
                end: usize::try_from(end).unwrap_or(usize::MAX),
            },
        }),
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
        | PeerPresencePayload::Person { .. }
        | PeerPresencePayload::Shell(_) => None,
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    const WORKSPACE: &str = "6b3f9a10-0000-4000-8000-00000000000c";
    const APP: &str = "so.tilo.notes";

    fn routes() -> BTreeSet<DocumentRoute> {
        BTreeSet::from([DocumentRoute {
            app_id: APP.to_owned(),
            cluster_id: WORKSPACE.to_owned(),
            record: "notes".to_owned(),
        }])
    }

    fn cursor() -> PresenceKey {
        PresenceKey::Pointer {
            space: collab_types::presence::PointerSpace::Tile {
                workspace: WORKSPACE.to_owned(),
                tab: 4,
            },
        }
    }

    /// A page's editor cursor goes out as the app's presence frame and comes
    /// back as the toolkit's cursor event: nothing of the old presence
    /// message is left in between.
    #[test]
    fn an_editors_pointer_rides_the_presence_frame_both_ways() {
        let frame = app_presence(
            WORKSPACE,
            APP,
            &cursor(),
            &PresencePayload::Pointer { x: 12.5, y: 40.0 },
        )
        .expect("a pointer frame");
        let body = frame.encode().expect("a presence frame encodes");
        let said = Presence::decode(&body).expect("the frame reads back");
        let event = editor_presence(&routes(), [7; 32], said).expect("the editor's cursor");
        let CollabEvent::RemoteCursorMoved { user_id, x, y } = event else {
            panic!("a pointer is a cursor move: {event:?}");
        };
        assert_eq!(user_id, hex::encode([7; 32]));
        assert_eq!((x, y), (12.5, 40.0));
    }

    /// A caret under the same key arrives as the editor's own presence, with
    /// the key the app gave it.
    #[test]
    fn an_editors_caret_carries_its_key() {
        let said = Presence::app(
            APP,
            WORKSPACE,
            cursor(),
            PeerPresencePayload::Caret { pos: 9 },
        );
        let event = editor_presence(&routes(), [7; 32], said).expect("the editor's caret");
        let CollabEvent::RemotePresenceUpdate {
            user_id,
            key,
            payload,
        } = event
        else {
            panic!("a caret is a presence update: {event:?}");
        };
        assert_eq!(user_id, hex::encode([7; 32]));
        assert_eq!(key, cursor());
        assert_eq!(payload, PresencePayload::Caret { pos: 9 });
    }

    /// Another app's presence, and the shell's, is not the editor's: an app
    /// draws nothing from a frame about a route it does not hold.
    #[test]
    fn another_apps_presence_is_not_the_editors() {
        let said = Presence::pointing(WORKSPACE, 3, 1.0, 2.0);
        assert!(editor_presence(&routes(), [7; 32], said).is_none());
        let elsewhere = Presence::app(
            APP,
            "07848ffe-1898-4f72-8419-0b54c93e6dad",
            cursor(),
            PeerPresencePayload::Pointer { x: 1.0, y: 2.0 },
        );
        assert!(editor_presence(&routes(), [7; 32], elsewhere).is_none());
    }
}
