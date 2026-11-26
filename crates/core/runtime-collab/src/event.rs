//! The events an application's collaboration endpoint receives from the
//! session: record changes, presence, actions, invocations and the
//! session-local control the broker routes to one connection.
//!
//! The vocabulary is shared by the toolkit's endpoint and by the session's
//! brokers, which fan these events out without the toolkit. One payload
//! exists only at an SDK endpoint, which mints it itself and lets no
//! application construct one: a committed-history capture.
//! [`EndpointOwnership`] names what stands in its place, so a broker uses
//! [`NoOwner`], where that variant cannot occur, and the toolkit fills in
//! its own ownership type.

use crate::route::{KeyAccessState, RouteSpec};
use collab_types::{
    DocumentMutation,
    presence::{PresenceKey, PresencePayload},
};

/// The payloads only an SDK endpoint can hold, which it mints itself.
pub trait EndpointOwnership: Clone + std::fmt::Debug {
    /// A disposable capture of a route's committed history, from the
    /// admitted SDK channel itself.
    type CommittedRead: Clone + std::fmt::Debug;
}

/// The ownership of a party that holds no SDK payloads, such as a broker:
/// the events that carry them cannot occur.
#[derive(Clone, Copy, Debug)]
pub enum NoOwner {}

impl EndpointOwnership for NoOwner {
    type CommittedRead = std::convert::Infallible;
}

/// One event a collaboration endpoint receives.
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "the layout every app matches on; boxing a payload would change each of those matches"
)]
pub enum CollabEvent<C: EndpointOwnership = NoOwner> {
    /// Original committed history from this SDK channel, applied by its document owner.
    CommittedDocument {
        route: RouteSpec,
        read: C::CommittedRead,
    },
    Context {
        permissions: Vec<String>,
        /// What each app installed on this device is called, in each language
        /// its package names it in: what a history row names another app's
        /// change by, never an id. Session-local presentation.
        apps: Vec<collab_types::history::NamedApp>,
    },
    /// Records of this application's trees (a chain's next part, a
    /// container's child) that a peer wrote and no window here holds:
    /// whatever follows those trees holds them. Session-local.
    ResourcesArrived { records: Vec<String> },
    /// The records another device wrote into this workspace's store. A window that keeps no
    /// document of its own, whose views read the store a record at a time, reads again what it
    /// read of these and nothing of the records not named: every view of a resource shows the
    /// change without re-reading the workspace. A window that holds the documents themselves
    /// has applied them already and needs no telling. Session-local.
    RecordsMoved { records: Vec<String> },
    /// The workspace's hosts sent everything they hold of the on-demand
    /// record on `route` (`neighbour_wire::document_sync`) this endpoint read,
    /// and it was not among it, or no host of the workspace is connected:
    /// what reads it shows it unavailable until it arrives, which its
    /// committed read delivers as ever. Session-local; minted by the SDK
    /// endpoint from its committed read.
    ResourceUnavailable { route: RouteSpec },
    /// Session chrome asked this app to bind its viewport/view mode to a
    /// member, or release that binding. This is local session control:
    /// it is never published to collaboration peers.
    FollowChanged {
        peer_id: Option<tilo_actions::MemberId>,
    },
    /// The shell asked this instance for a step of moving its window to
    /// another workspace. Session-local control; never from a peer.
    Handoff {
        request: collab_types::handoff::HandoffRequest,
    },
    RemoteCursorMoved {
        member: collab_types::MemberId,
        x: f32,
        y: f32,
    },
    RemotePresenceUpdate {
        member: collab_types::MemberId,
        key: PresenceKey,
        payload: PresencePayload,
    },
    /// What this endpoint can do with one route's end-to-end encrypted
    /// content changed: it was admitted, is waiting for access (a new device
    /// before its welcome arrives), was removed, or cannot join: what the
    /// content it stores is sealed under. Apps show the state.
    KeyAccess {
        route: RouteSpec,
        state: KeyAccessState,
    },
    /// Changes of the record on `route` this endpoint lacked, which the device's store sent it
    /// (the store command `RecordChanges`): what the endpoint merges into its own copy of the
    /// record.
    Document {
        route: RouteSpec,
        mutation: DocumentMutation,
    },
    /// One capability-checked invocation addressed to this application
    /// connection by the session action broker.
    Invocation {
        id: u64,
        invocation: tilo_actions::InvocationId,
        resource: String,
        action: String,
        argument: Option<String>,
        actor: tilo_actions::Actor,
        /// Broker-owned session provenance. The concrete application
        /// resource and action remain authoritative; this only supplies the
        /// surrounding purpose and work-group for durable history.
        context: Option<Box<tilo_actions::ActionContext>>,
    },
    /// The invocation owner requested cooperative cancellation. Delivered at
    /// most once; the app-side job decides the next safe stopping boundary.
    InvocationCancellation { id: u64 },
    /// The session's answer to a the toolkit's `CollabRequest::RemoteAction`.
    RemoteAction {
        message: tilo_actions::remote::RemoteActionMessage,
    },
    /// Private admission/status or refusal, never a document publication permit.
    LocalDocumentActionReported {
        request_id: u64,
        result: Result<crate::PublishedInvocationObservation, crate::LocalDocumentActionError>,
    },
    WorkspaceControl {
        session_id: u64,
        event: crate::WorkspaceControlEvent,
    },
    /// Content the record on `route` references arrived: an answer to
    /// the toolkit's `CollabRequest::WantContent`, or content another app on the record
    /// offered. The bytes are verified against `hash` before use.
    DocumentContent {
        route: RouteSpec,
        hash: String,
        bytes: Vec<u8>,
    },
    /// Where a the toolkit's `CollabRequest::WantContent` of `hash` on `route` has got
    /// to while the content is not here: fetched from a connected device
    /// ([`ContentState::Pending`](tilo_content::ContentState::Pending)),
    /// waiting for one that could hand it over to connect, or unavailable
    /// from every connected one. It changes as devices come and go.
    DocumentContentState {
        route: RouteSpec,
        hash: String,
        state: tilo_content::ContentState,
    },
    /// The store's answer to the app's resource command `request_id`, in the store's encoding
    /// (`collab_types::store::wire::Reply`).
    ResourceCommandResult {
        request_id: u64,
        result: Result<Vec<u8>, String>,
    },
    /// What the host says of the actions the window last named
    /// (`CollabRequest::DecideShownActions`) now, replacing what it said before. Sent again
    /// whenever grants change one. They decide how the window shows its controls only; the host
    /// admits each invocation itself when it is called.
    ActionVerdicts {
        verdicts: Vec<crate::shown_actions::ActionDecision>,
    },
}

impl<C: EndpointOwnership> CollabEvent<C> {
    /// The changes a document event carries; `None` for every other event.
    #[must_use]
    pub const fn document_mutation(&self) -> Option<&DocumentMutation> {
        match self {
            Self::Document { mutation, .. } => Some(mutation),
            _ => None,
        }
    }
}
