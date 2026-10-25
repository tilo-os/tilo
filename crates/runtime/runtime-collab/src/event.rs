//! The events an application's collaboration endpoint receives from the
//! session: document changes, presence, actions, invocations and the
//! session-local control the broker routes to one connection.
//!
//! The vocabulary is shared by the toolkit's endpoint and by the session's
//! brokers, which fan these events out without the toolkit. Two payloads
//! exist only at an SDK endpoint, which mints them itself and lets no
//! application construct one: a committed-history capture and a managed
//! invocation result. [`EndpointCustody`] names what stands in their place,
//! so a broker uses [`NoCustody`], where those variants cannot occur, and the
//! toolkit fills in its own custody types.

use crate::BrokerActionSurface;
use crate::route::{ChannelAccessState, RouteSpec};
use collab_types::DocumentMutation;
use collab_types::SignedOp;
use collab_types::presence::{Collaborator, PresenceKey, PresencePayload};

/// The payloads only an SDK endpoint can hold, which it mints itself.
pub trait EndpointCustody: Clone + std::fmt::Debug {
    /// A disposable capture of a route's committed history, from the
    /// admitted SDK channel itself.
    type CommittedRead: Clone + std::fmt::Debug;
    /// The custody of a managed invocation attempt's result, which only the
    /// sender's own barrier can acknowledge.
    #[cfg(unix)]
    type ManagedResult: Clone + std::fmt::Debug;
}

/// The custody of a party that holds no SDK payloads, such as a broker:
/// the events that carry them cannot occur.
#[derive(Clone, Copy, Debug)]
pub enum NoCustody {}

impl EndpointCustody for NoCustody {
    type CommittedRead = std::convert::Infallible;
    #[cfg(unix)]
    type ManagedResult = std::convert::Infallible;
}

/// One event a collaboration endpoint receives.
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "the layout every app matches on; boxing a payload would change each of those matches"
)]
pub enum CollabEvent<C: EndpointCustody = NoCustody> {
    SelectedDiscoveryResult {
        request_id: u64,
        result: Result<crate::open_selected::discovery::Reply, String>,
    },
    OpenSelectedV2Result {
        request_id: u64,
        result: Result<crate::open_selected::v2::ObservedReply, String>,
    },
    OpenSelectedResult {
        request_id: u64,
        result: Result<crate::open_selected::RootReceipt, String>,
    },

    LocalFileClipboardResult {
        request_id: u64,
        result: Result<crate::file_clipboard::Reply, String>,
    },
    /// Original committed history from this SDK channel, applied by its document owner.
    CommittedDocument {
        route: RouteSpec,
        read: C::CommittedRead,
    },
    Context {
        collaborators: Vec<Collaborator>,
        permissions: Vec<String>,
    },
    /// The delegated agents on `cluster_id`'s roster whose person's root
    /// signed the delegation for exactly that device, as the session
    /// verified them: who the app's history names for changes their own
    /// device keys signed. Session-local and presentation only.
    HistoryAgents {
        cluster_id: String,
        agents: Vec<collab_types::history::RosterAgent>,
    },
    /// Records of this application's trees (a chain's next part, a
    /// container's child) that a peer wrote and no window here holds:
    /// whatever follows those trees holds them. Session-local.
    RecordsArrived {
        records: Vec<String>,
    },
    /// Session chrome asked this app to bind its viewport/view mode to a
    /// participant, or release that binding. This is local session control:
    /// it is never published to collaboration peers.
    FollowChanged {
        peer_id: Option<String>,
    },
    /// The broker selected this connection as the sole snapshot responder for
    /// a late joiner on the same document route.
    DocumentSnapshotRequested {
        route: RouteSpec,
    },
    /// The shell asked this instance for a step of moving its window to
    /// another workspace. Session-local control; never from a peer.
    Handoff {
        request: collab_types::handoff::HandoffRequest,
    },
    RemoteCursorMoved {
        user_id: String,
        x: f32,
        y: f32,
    },
    RemotePresenceUpdate {
        user_id: String,
        key: PresenceKey,
        payload: PresencePayload,
    },
    /// A typed action another app in this room published, delivered to every
    /// connection. A receiver decodes it via its own
    /// [`ActionRegistry`](tilo_actions::ActionRegistry); an action for a
    /// different app decodes to nothing, making room-wide fan-out safe.
    ///
    /// `from_user_id` is who *sent* the frame, not necessarily who the
    /// action says authored it.
    ActionArrived {
        type_id: String,
        payload: Vec<u8>,
        from_user_id: String,
    },
    /// What this endpoint can do with one route's end-to-end encrypted
    /// content changed: it was admitted, is waiting for access (a new device
    /// before its welcome arrives), was removed, or cannot join. While a
    /// route is not open the SDK holds edits and incoming ciphertext on the
    /// device instead of dropping them; apps show the state.
    ChannelAccess {
        route: RouteSpec,
        state: ChannelAccessState,
    },
    /// A validated signed document envelope from another peer. For cleartext
    /// routes the mutation kind is inside the outer signed payload. For an
    /// MLS route, `envelope.payload` remains the original signed ciphertext
    /// and `decrypted_payload` holds endpoint-only plaintext after the inner
    /// MLS authentication succeeds. A mutation too large for one frame
    /// arrives as several signed segments; the SDK delivers it once, after
    /// every segment arrived and the reassembled bytes matched their signed
    /// digest, with `envelope` set to the final segment.
    Document {
        envelope: Box<SignedOp>,
        from_user_id: String,
        /// Endpoint-verified typed mutation bytes when they differ from
        /// `envelope.payload`: MLS plaintext, or a mutation reassembled from
        /// segments. `None` for single-frame cleartext envelopes (read
        /// directly from `envelope.payload`) and for broker events, which
        /// never possess MLS keys.
        decrypted_payload: Option<Vec<u8>>,
    },
    /// Response to a the toolkit's `CollabRequest::FetchScrollback`: a page of
    /// previously-validated ops plus `exhausted`.
    ///
    /// Targeted send — unlike every other event here, the broker writes
    /// this only to the connection that issued the matching request.
    ScrollbackBatch {
        /// Echoes `FetchScrollback::artifact` so multiple in-flight
        /// fetches route to the right history view.
        artifact: collab_types::ArtifactId,
        /// Ascending seq order. Empty when the broker has no matching
        /// range (typically evicted by compaction).
        ops: Vec<Box<SignedOp>>,
        /// Endpoint-decrypted payload parallel to `ops`. Brokers leave this
        /// empty because they have no MLS keys; SDK endpoints populate one
        /// slot per op and drop undecryptable encrypted entries.
        decrypted_payloads: Vec<Option<Vec<u8>>>,
        /// Oldest `seq` the host returned, before the endpoint dropped ops
        /// it could not use yet (an incomplete segment), so paging resumes
        /// below it rather than re-fetching or stopping early.
        oldest_seq: Option<u64>,
        exhausted: bool,
    },
    /// One capability-checked invocation addressed to this application
    /// connection by the session action broker.
    Invocation {
        id: u64,
        invocation: tilo_actions::InvocationId,
        resource: String,
        action: String,
        argument: Option<String>,
        actor: String,
        /// Broker-owned task/session provenance. The concrete application
        /// resource and action remain authoritative; this only supplies the
        /// surrounding purpose and work-group for durable history.
        context: Option<Box<tilo_actions::ActionContext>>,
    },
    /// The invocation owner requested cooperative cancellation. Delivered at
    /// most once; the app-side job decides the next safe stopping boundary.
    InvocationCancellation {
        id: u64,
    },
    /// The session's answer to a the toolkit's `CollabRequest::RemoteAction`.
    RemoteAction {
        message: tilo_actions::remote::RemoteActionMessage,
    },
    ActionCatalogue {
        surfaces: Vec<BrokerActionSurface>,
    },
    ActionDirectoryPage {
        request_id: u64,
        result: Result<crate::ActionDirectoryPage, String>,
    },
    PublishedActionResolution {
        request_id: u64,
        result: Result<crate::ActionDirectoryResolution, String>,
    },
    /// Original attempt custody remains with this result through final clone
    /// destruction. Only the sender's physical barrier can acknowledge it.
    #[cfg(unix)]
    ManagedInvocationObservation {
        result: C::ManagedResult,
    },
    /// Private admission/status or refusal, never a document publication permit.
    LocalDocumentActionReported {
        request_id: u64,
        result: Result<crate::PublishedInvocationObservation, crate::LocalDocumentActionError>,
    },
    PublishedInvocationResult {
        request_id: u64,
        result: Result<(u64, tilo_actions::InvocationUpdate), String>,
    },
    InstalledApplicationLaunchResult {
        request_id: u64,
        result: Result<(u64, tilo_actions::InvocationUpdate), String>,
    },
    /// A run addressed to this admitted agent. The envelope is broker-minted
    /// and contains a lease, not a caller-selected authority token.
    TaskRun(crate::TaskRunEnvelope),
    TaskActionInvocationResult {
        request_id: u64,
        result: Result<(u64, tilo_actions::InvocationUpdate), String>,
    },
    TaskRunCancellation {
        request_id: u64,
        result: Result<bool, String>,
    },
    TaskRunControl {
        request_id: u64,
        control: crate::TaskRunControl,
    },
    TaskRunReportResult {
        request_id: u64,
        result: Result<(), String>,
    },
    WorkspaceControl {
        session_id: u64,
        event: crate::WorkspaceControlEvent,
    },
    NativeMediaCommandResult {
        request_id: u64,
        result: Result<crate::native_media::Snapshot, String>,
    },
    /// Content the record on `route` references arrived: an answer to
    /// the toolkit's `CollabRequest::WantContent`, or content another app on the record
    /// offered. The bytes are verified against `hash` before use.
    RecordContent {
        route: RouteSpec,
        hash: String,
        bytes: Vec<u8>,
    },
    /// Where a the toolkit's `CollabRequest::WantContent` of `hash` on `route` has got
    /// to while the content is not here: fetched from a connected device
    /// ([`ContentState::Pending`](tilo_content::ContentState::Pending)),
    /// waiting for one that could hand it over to connect, or unavailable
    /// from every connected one. It changes as devices come and go.
    RecordContentState {
        route: RouteSpec,
        hash: String,
        state: tilo_content::ContentState,
    },
    RealtimeCommandResult {
        request_id: u64,
        result: Result<collab_types::realtime::RealtimeReply, String>,
    },
    NearbyTransferCommandResult {
        request_id: u64,
        result: Result<crate::NearbyTransferReply, String>,
    },
    /// What a Nearby Send snapshot would show this app may have changed: a
    /// device came or went, an offer arrived, a transfer moved on. It names
    /// nothing; the app asks for a fresh
    /// [`NearbyTransferCommand::Snapshot`](crate::NearbyTransferCommand::Snapshot)
    /// instead of asking on a timer. The first arrives once the route joins,
    /// so it also starts the app's view. Only the certified Files route
    /// receives these.
    NearbyTransferChanged,
    /// What a speaker snapshot would show may have changed: a device came or
    /// went, a member answered, a calibration moved on. It names nothing;
    /// Settings asks for a fresh
    /// [`SpeakerCommand::Snapshot`](crate::SpeakerCommand::Snapshot)
    /// instead of asking on a timer. The first arrives once the route joins,
    /// so it also starts the view. Only the certified Settings route
    /// receives these.
    SpeakersChanged,
    /// The drives the shell lists may have changed: one was plugged in,
    /// taken out, opened or closed. It names nothing; Files asks the tools
    /// service for the list again instead of asking on a timer. The first
    /// arrives once the route joins, so it also fetches the first list. Only
    /// the certified Files route receives these.
    DrivesChanged,
    Realtime(collab_types::realtime::RealtimeEvent),
}

impl<C: EndpointCustody> CollabEvent<C> {
    /// Decode the typed payload of a validated document event. Returns `None`
    /// for presence, context, action, and scrollback events.
    pub fn document_mutation(
        &self,
    ) -> Option<Result<DocumentMutation, collab_types::DocumentMutationError>> {
        let Self::Document {
            envelope,
            decrypted_payload,
            ..
        } = self
        else {
            return None;
        };
        Some(DocumentMutation::decode(
            decrypted_payload.as_deref().unwrap_or(&envelope.payload),
        ))
    }
}
