//! Toolkit-side collab client: a synchronous, non-blocking `send` / `poll`
//! pair trading typed [`CollabRequest`] / [`CollabEvent`] frames, stable so
//! every downstream app keeps compiling while the transport underneath
//! changes.
//!
//! The manager delegates to the `runtime-collab` Cap'n Proto SDK on a
//! shovel thread it owns, bridging winit-side `send` / `poll` to the SDK
//! over a pair of bounded mpsc channels. Durable traffic fails the bridge
//! closed on overload; only transient presence may be shed. On
//! [`runtime_portal::NotAvailable`] it enters "disabled" mode instead of
//! panicking — `poll()` returns empty, `send()` is a no-op, no thread is
//! spawned.
//!

use uuid::Uuid;

/// Hard fail-closed timeout for an RPC request waiting to be handled on the
/// application's owner thread.  The UI never blocks for it: the Cap'n Proto
/// shovel awaits this reply on its own `LocalSet`.  A stopped app must look
/// unavailable to the broker rather than leaving an RPC worker pinned.
const ACTION_DIRECTORY_REPLY_TIMEOUT: Duration = Duration::from_secs(2);
/// A disconnected/slow UI must apply backpressure to broker callbacks rather
/// than allow an unbounded cross-thread queue to become a memory `DoS`.
const ACTION_DIRECTORY_REQUEST_QUEUE: usize = 64;

/// Reserved `invokePublishedAction.peerId` meaning "the application
/// connection making this request". Live broker peer ids are allocated from
/// zero upward; the broker never assigns this sentinel.
pub const CURRENT_CONNECTION_PEER_ID: u64 = u64::MAX;

pub use collab_types::DocumentMutation;
use collab_types::SignedOp;

use crate::presence::PresenceKey;

use runtime_portal::RawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::LocalSet;

/// How long an app waits for the collab connect path to report before
/// opening without it, spent on the main thread ahead of the first frame.
/// Every step before the report is a local syscall, so reaching this means
/// something upstream is wedged.
const SHOVEL_INIT_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum number of UI-to-shovel commands waiting behind a slow broker.
/// This deliberately caps commands, rather than bytes, because the protocol
/// already bounds its payloads at each broker boundary.
const SDK_COMMAND_QUEUE: usize = 256;
/// Maximum number of shovel-to-UI events waiting behind a busy event loop.
const SDK_EVENT_QUEUE: usize = 256;
/// Aggregate retained payload budget for each cross-thread queue. This is
/// independent of the item-count cap because a single document snapshot or
/// invocation argument may be large.
const SDK_COMMAND_QUEUE_BYTES: usize = 4 * 1024 * 1024;
const SDK_EVENT_QUEUE_BYTES: usize = 4 * 1024 * 1024;

/// Inert, app-owned resume projection sent only to the local shell.
///
/// The bytes are deliberately opaque to the toolkit so each app can version
/// its CRDT-derived schema. The shell validates size and path redaction before
/// retaining them for same-identity continuation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContinuationMetadata {
    pub schema_version: u16,
    pub bytes: Vec<u8>,
}

/// Decode the launcher-only resume payload. Invalid input is ignored so an
/// app outside qOS or an older sender opens normally.
pub fn continuation_restore_env() -> Option<ContinuationMetadata> {
    let bytes = hex::decode(std::env::var("QOS_CONTINUITY_RESUME_HEX").ok()?).ok()?;
    let schema_version = std::env::var("QOS_CONTINUITY_RESUME_SCHEMA")
        .ok()?
        .parse()
        .ok()?;
    (!bytes.is_empty() && bytes.len() <= 32 * 1024).then_some(ContinuationMetadata {
        schema_version,
        bytes,
    })
}

#[derive(Debug, Clone)]
pub enum CollabRequest {
    Hello {
        app_id: String,
        cluster_id: Uuid,
        /// Which **record** ([`crate::record`]) this connection carries, when
        /// adopted. The broker delivers payloads only to connections
        /// announcing the same record, so two people editing different
        /// files in one room don't merge each other's state; `None` means
        /// one document per `(app, room)`.
        record: Option<String>,
        /// Name on the caret pill; `None` gets a synthetic `peer-<n>`.
        /// Informational only — never used for routing or grants.
        handle: Option<String>,
    },
    /// Complete document state. The SDK wraps this mutation in a signed
    /// envelope from the certified launcher identity before transmission.
    DocumentSnapshot {
        data: Vec<u8>,
    },
    /// Incremental document changes. The SDK wraps this mutation in a signed
    /// envelope from the certified launcher identity before transmission.
    DocumentIncremental {
        data: Vec<u8>,
    },
    /// Outbound presence update — pointer moved, caret changed, selection
    /// shifted, or arrived/left a keyed object. Fanned out to every
    /// connected peer as `CollabEvent::RemotePresenceUpdate`.
    Presence {
        key: PresenceKey,
        payload: PresencePayload,
    },
    /// Signed incremental CRDT mutation. The broker validates against the
    /// channel's role table before fanning out; invalid envelopes are
    /// dropped and logged, never reaching peers.
    ///
    /// `Box`ed to keep the discriminant thin on the stack — `SignedOp` is
    /// large (32-byte pubkey + 64-byte sig + payload + UUIDs).
    SignedOperation {
        envelope: Box<SignedOp>,
    },
    /// Late-join scrollback fetch: up to `max` ops with `seq < before_seq`,
    /// answered with one [`CollabEvent::ScrollbackBatch`].
    ///
    /// Apps page by repeating with `before_seq` set to the lowest seq from
    /// the previous batch until `exhausted == true`, then fall back to the
    /// cooperative path (broker queries gossip peers).
    FetchScrollback {
        artifact: collab_types::ArtifactId,
        /// Exclusive upper bound on `seq`.
        before_seq: u64,
        /// Page size cap; chrome UX bounds pages to ~50 ops.
        max: u16,
    },
    /// Replace this application's complete exported resource/action surface.
    PublishActions {
        app_id: String,
        actions: Vec<ExportedAction>,
    },
    /// App-owned CRDT state projected for same-identity continuation. The
    /// local broker authenticates the route and stores this only for shell
    /// capture; it is never a room action or a restore command.
    ReportContinuation {
        app_id: String,
        schema_version: u16,
        metadata: Vec<u8>,
    },
    /// Remove the last continuation report after the app can no longer
    /// produce a safe projection.
    ClearContinuation {
        app_id: String,
    },
    OpenWorkspaceControl {
        session_id: u64,
        target_device: [u8; 32],
    },
    WorkspaceControlInput {
        session_id: u64,
        event: runtime_collab::WorkspaceControlInput,
    },
    CloseWorkspaceControl {
        session_id: u64,
    },
    /// Report the terminal result of a directed invocation.
    CompleteInvocation {
        id: u64,
        code: String,
        message: String,
    },
    UpdateInvocation {
        id: u64,
        update: qos_actions::InvocationUpdate,
    },
    /// Terminal receipt for an Effect initiated by this app's local UI.
    RecordLocalEffect {
        address: qos_actions::ActionAddress,
        update: qos_actions::InvocationUpdate,
    },
    InvokePublishedAction {
        request_id: u64,
        peer_id: u64,
        resource: String,
        action: String,
        arguments: serde_json::Map<String, serde_json::Value>,
        confirmation: serde_json::Value,
    },
    PublishedInvocationStatus {
        request_id: u64,
        id: u64,
    },
    CancelPublishedInvocation {
        request_id: u64,
        id: u64,
    },
    LaunchPublishedApplication {
        request_id: u64,
        app_id: String,
    },
    /// Lifecycle record for a broker-minted task lease. Only an admitted
    /// agent connection can make this request; the broker checks the lease.
    ReportTaskRun {
        run_id: u64,
        lease: String,
        update: runtime_collab::TaskRunUpdate,
    },
    /// Invoke an already permission-filtered published action under the
    /// broker-owned task context. `request_id` correlates the async reply.
    InvokeTaskAction {
        request_id: u64,
        run_id: u64,
        lease: String,
        peer_id: u64,
        resource: String,
        action: String,
        arguments: serde_json::Map<String, serde_json::Value>,
    },
    /// Poll the native Task Center's cooperative cancellation bit for a
    /// lease. It cannot cancel a task and carries no task state itself.
    TaskRunCancellation {
        request_id: u64,
        run_id: u64,
        lease: String,
    },
}

/// A broker directory request awaiting main-thread authority.  This crosses
/// threads as inert strings and budgets only; no widget reference or Cap'n
/// Proto handle ever leaves the UI/collaboration boundary.
pub enum ActionDirectoryRequest {
    Query {
        query: runtime_collab::ActionDirectoryQuery,
        reply: tokio::sync::oneshot::Sender<runtime_collab::ActionDirectoryPage>,
    },
    Resolve {
        revision: String,
        resource: String,
        action: String,
        reply: tokio::sync::oneshot::Sender<runtime_collab::ActionDirectoryResolution>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportedInput {
    None,
    Text {
        prompt: String,
        placeholder: String,
        wants_content: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedAction {
    pub resource: String,
    pub action: String,
    pub resource_kind: String,
    pub resource_label: String,
    pub action_label: String,
    pub description: Option<String>,
    pub operation_kind: String,
    pub params_json: String,
    pub contract_json: String,
    pub input: ExportedInput,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct BrokerActionSurface {
    pub peer_id: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub record_id: String,
    pub live: bool,
    pub actions: Vec<BrokerActionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct BrokerActionDescriptor {
    pub resource: String,
    pub action: String,
    pub resource_kind: String,
    pub resource_label: String,
    pub action_label: String,
    pub description: Option<String>,
    pub operation_kind: qos_actions::OperationKind,
    pub params: Vec<qos_actions::ActionParam>,
    pub contract: qos_actions::ActionContract,
    pub input: BrokerActionInput,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct BrokerActionInput {
    pub kind: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub placeholder: String,
    #[serde(default)]
    pub wants_content: bool,
}

#[derive(serde::Deserialize)]
struct BrokerActionCatalogueWire {
    surfaces: Vec<BrokerActionSurface>,
}

/// An explicit typed collaboration route.
///
/// A manager has one primary route (announced by its initial `Hello`) and may
/// retain additional routes for discovery or cross-app work. Callers must name
/// an additional route again when publishing, which prevents a cluster-wide
/// discovery subscription from becoming an accidental mutation target.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteSpec {
    pub app_id: String,
    pub cluster_id: Uuid,
    pub record: Option<String>,
}

impl RouteSpec {
    pub fn cluster(cluster_id: Uuid) -> Self {
        Self {
            app_id: String::new(),
            cluster_id,
            record: None,
        }
    }

    pub fn app(app_id: impl Into<String>, cluster_id: Uuid) -> Self {
        Self {
            app_id: app_id.into(),
            cluster_id,
            record: None,
        }
    }

    pub fn record(app_id: impl Into<String>, cluster_id: Uuid, record: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
            cluster_id,
            record: Some(record.into()),
        }
    }

    fn channel_id(&self) -> String {
        channel_key::format(
            &self.app_id,
            &self.cluster_id.to_string(),
            self.record.as_deref().unwrap_or(""),
        )
    }
}

#[derive(Debug, Clone)]
pub enum CollabEvent {
    Context {
        collaborators: Vec<Collaborator>,
        permissions: Vec<String>,
    },
    /// Session chrome asked this app to bind its viewport/view mode to a
    /// participant, or release that binding. This is local session control:
    /// it is never published to collaboration peers.
    FollowChanged {
        peer_id: Option<String>,
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
    /// [`ActionRegistry`](qos_actions::ActionRegistry); an action for a
    /// different app decodes to nothing, making room-wide fan-out safe.
    ///
    /// `from_user_id` is who *sent* the frame, not necessarily who the
    /// action says authored it.
    ActionArrived {
        type_id: String,
        payload: Vec<u8>,
        from_user_id: String,
    },
    /// A validated signed document envelope from another peer. For cleartext
    /// routes the mutation kind is inside the outer signed payload. For an
    /// MLS route, `envelope.payload` remains the original signed ciphertext
    /// and `decrypted_payload` holds endpoint-only plaintext after the inner
    /// MLS authentication succeeds.
    Document {
        envelope: Box<SignedOp>,
        from_user_id: String,
        /// Endpoint-decrypted typed mutation bytes. `None` for cleartext
        /// envelopes (read directly from `envelope.payload`) and for broker
        /// events, which never possess MLS keys.
        decrypted_payload: Option<Vec<u8>>,
    },
    /// Response to a [`CollabRequest::FetchScrollback`]: a page of
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
        exhausted: bool,
    },
    /// One capability-checked invocation addressed to this application
    /// connection by the session action broker.
    Invocation {
        id: u64,
        resource: String,
        action: String,
        argument: Option<String>,
        actor: String,
        /// Broker-owned task/session provenance. The concrete application
        /// resource and action remain authoritative; this only supplies the
        /// surrounding purpose and work-group for durable history.
        context: Option<Box<qos_actions::ActionContext>>,
    },
    /// The invocation owner requested cooperative cancellation. Delivered at
    /// most once; the app-side job decides the next safe stopping boundary.
    InvocationCancellation {
        id: u64,
    },
    ActionCatalogue {
        surfaces: Vec<BrokerActionSurface>,
    },
    PublishedInvocationResult {
        request_id: u64,
        result: Result<(u64, qos_actions::InvocationUpdate), String>,
    },
    /// A run addressed to this admitted agent. The envelope is broker-minted
    /// and contains a lease, not a caller-selected authority token.
    TaskRun(runtime_collab::TaskRunEnvelope),
    TaskActionInvocationResult {
        request_id: u64,
        result: Result<(u64, qos_actions::InvocationUpdate), String>,
    },
    TaskRunCancellation {
        request_id: u64,
        result: Result<bool, String>,
    },
    WorkspaceControl {
        session_id: u64,
        event: runtime_collab::WorkspaceControlEvent,
    },
}

impl CollabEvent {
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

#[derive(Debug, Clone, PartialEq)]
pub enum PresencePayload {
    Pointer {
        x: f32,
        y: f32,
    },
    Caret {
        pos: usize,
    },
    Selection {
        start: usize,
        end: usize,
    },
    /// The local user just inserted `start..end` under scope `key`; peers
    /// render a fading "just typed" swipe (see
    /// [`crate::presence::PresenceCtx::record_peer_typing`]). Unlike
    /// `Caret`/`Selection` this is an *event*, published once and decayed
    /// on the receiver.
    Typing {
        start: usize,
        end: usize,
    },
    Arrived,
    Left,
    /// A room-scoped, transient follow relationship announced by the
    /// authenticated sender. It is a visual presence cue only; the local
    /// Follow command remains private to the follower's session.
    Following {
        target: Option<String>,
    },
    /// Truthful action/resource activity. Identity is supplied by the
    /// authenticated event envelope; this payload never describes a mouse or
    /// keyboard gesture and therefore cannot create a synthetic cursor.
    Interaction(Box<qos_actions::SemanticInteraction>),
}

#[derive(Debug, Clone)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    pub color: [f32; 3],
    /// Whether this participant is an agent rather than a person.
    pub is_agent: bool,
    /// Whether this row is the receiving machine's own participant.
    /// Apps can't derive this themselves: `id` is the address *other*
    /// machines use, and a machine never announces its own to its own apps.
    pub is_self: bool,
}

/// Failure modes for [`CollaborationManager::send_signed_op`].
/// `NoIdentity` is the common case — call before the shell provisions an
/// identity — and callers can fall back to plain `DocumentIncremental`.
#[derive(Debug)]
pub enum SignedSendError {
    /// No [`SigningIdentity`] bound (see [`CollaborationManager::with_identity`]).
    NoIdentity,
    /// The identity has signed every representable sequence number. Reusing
    /// one would make a fresh edit indistinguishable from a replay attack, so
    /// the identity must be rotated before any more operations are signed.
    SequenceExhausted,
    /// Canonical-bytes serialisation of the envelope failed; in practice
    /// only fires for unrepresentable values.
    Canonical(collab_types::CanonicalError),
}

impl std::fmt::Display for SignedSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignedSendError::NoIdentity => {
                f.write_str("CollaborationManager has no signing identity bound")
            }
            SignedSendError::SequenceExhausted => {
                f.write_str("collaboration signing sequence exhausted; rotate the identity")
            }
            SignedSendError::Canonical(e) => write!(f, "canonical encoder failed: {e}"),
        }
    }
}

impl std::error::Error for SignedSendError {}

/// Identity-side handle used by [`CollaborationManager::send_signed_op`]
/// to mint signed envelopes. The secret key stays private — not
/// re-exported on the manager's public surface — so app code can only
/// sign via the manager's helper, never handle the raw secret.
pub struct SigningIdentity {
    /// Local secret half — never serialised, never logged.
    pub(crate) secret_key: ed25519_dalek::SigningKey,
    /// Public half — embedded as the `author` field on every signed op.
    pub public_key: collab_types::Ed25519PublicKey,
    /// Role attestation the broker looks up ops against. One identity may
    /// hold attestations on multiple channels; this pins to one per manager.
    pub attestation_id: collab_types::AttestationId,
    /// Monotonic per-author seq, bumped on every `sign` so the validator's
    /// replay check (`seq > last_seen`) passes. Reusing a seq is the
    /// spec-defined replay attack.
    pub(crate) seq: u64,
}

impl SigningIdentity {
    /// Construct an identity from a fresh keypair and the attestation id
    /// that authorises it. Mostly used in tests; production code builds
    /// via the shell-side identity-loading helper.
    pub fn new(
        secret_key: ed25519_dalek::SigningKey,
        attestation_id: collab_types::AttestationId,
    ) -> Self {
        let public_key =
            collab_types::Ed25519PublicKey::from_bytes(secret_key.verifying_key().to_bytes());
        Self {
            secret_key,
            public_key,
            attestation_id,
            seq: 0,
        }
    }

    /// Build a fresh `SignedOp` envelope wrapping `payload`, bumping the
    /// internal seq so callers don't need to track sequence numbers.
    ///
    /// # Errors
    ///
    /// Returns [`SignedSendError::SequenceExhausted`] rather than ever reusing
    /// a sequence number, or [`SignedSendError::Canonical`] when `payload`
    /// fails to canonicalize for signing.
    pub fn sign_op(
        &mut self,
        payload: Vec<u8>,
        artifact: collab_types::ArtifactId,
        op_class: collab_types::OpClass,
    ) -> Result<collab_types::SignedOp, SignedSendError> {
        let timestamp = std::time::SystemTime::now();
        let sequence = self
            .seq
            .checked_add(1)
            .ok_or(SignedSendError::SequenceExhausted)?;
        let mut op = collab_types::SignedOp::new_unsigned(
            payload,
            artifact,
            self.public_key,
            self.attestation_id,
            op_class,
            timestamp,
            sequence,
        );
        collab_types::sign_op(&mut op, &self.secret_key).map_err(SignedSendError::Canonical)?;
        self.seq = sequence;
        Ok(op)
    }
}

/// Which transport the manager is speaking. Enum-per-mode instead of
/// trait-object'ing because the match cost on `send` / `poll` is
/// negligible and the code stays flat for a reader.
enum Backend {
    /// No collab service reachable. `poll` yields nothing, `send` is a
    /// black hole; every widget already handles this via
    /// [`CollaborationManager::is_connected`].
    Disabled,
    /// Talking Cap'n Proto to `runtime-collab`. The shovel thread owns
    /// the SDK client + subscriptions; winit-side send / poll bridge
    /// through bounded mpsc pairs.
    Sdk(SdkChannel),
}

/// Owned mpsc pair + shovel-thread handle for SDK mode. Both directions are
/// bounded. Durable overflow makes the bridge unavailable rather than
/// silently losing a document, action, or invocation transition.
///
/// Dropping the manager drops `outbound_tx`; the shovel thread's
/// `.recv()` then yields `None` and the thread exits. `_thread` is kept
/// only so the `JoinHandle` isn't detached.
struct SdkChannel {
    outbound_tx: tokio_mpsc::Sender<QueuedSdkCommand>,
    inbound_rx: tokio_mpsc::Receiver<QueuedCollabEvent>,
    state: BridgeState,
    /// The receiver stays on the application's owner thread.  RPC servers
    /// own only the cloneable sender, so a Cap'n Proto callback cannot touch
    /// widgets directly.
    action_directory_rx: mpsc::Receiver<ActionDirectoryRequest>,
    action_directory_tx: mpsc::SyncSender<ActionDirectoryRequest>,
    action_directory_published: bool,
    _thread: Option<thread::JoinHandle<()>>,
}

enum SdkCommand {
    Primary(CollabRequest),
    Join(RouteSpec),
    ToRoute(RouteSpec, CollabRequest),
    PrimaryReady(mpsc::SyncSender<Result<(), String>>),
    InstallEncryption {
        spec: RouteSpec,
        group: Box<channel_crypto::ChannelGroup>,
        reply: mpsc::SyncSender<Result<(), String>>,
    },
    PublishActionDirectory {
        app_id: String,
        requests: mpsc::SyncSender<ActionDirectoryRequest>,
    },
}

impl SdkCommand {
    /// Presence is visual, short-lived state. All other frames represent a
    /// durable state change, an ordered request, or a lifecycle result.
    fn is_transient_presence(&self) -> bool {
        matches!(
            self,
            Self::Primary(CollabRequest::Presence { .. })
                | Self::ToRoute(_, CollabRequest::Presence { .. })
        )
    }
}

/// Checked byte meter for queue admission. `None` means a size computation
/// overflowed and is treated exactly like durable overload.
#[derive(Default)]
struct ByteMeter(Option<usize>);

impl ByteMeter {
    fn add(&mut self, bytes: usize) {
        self.0 = self.0.and_then(|total| total.checked_add(bytes));
    }

    fn add_product(&mut self, count: usize, bytes_each: usize) {
        self.0 = self.0.and_then(|total| {
            count
                .checked_mul(bytes_each)
                .and_then(|bytes| total.checked_add(bytes))
        });
    }

    fn string_capacity(&mut self, value: &String) {
        self.add(value.capacity());
    }

    fn bytes(&mut self, value: &Vec<u8>) {
        self.add(value.capacity());
    }

    fn finish(self) -> Option<usize> {
        self.0
    }
}

fn json_bytes(value: &serde_json::Value, meter: &mut ByteMeter) {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
        serde_json::Value::String(value) => meter.string_capacity(value),
        serde_json::Value::Array(values) => {
            meter.add_product(values.capacity(), std::mem::size_of::<serde_json::Value>());
            for value in values {
                json_bytes(value, meter);
            }
        }
        serde_json::Value::Object(values) => json_map_bytes(values, meter),
    }
}

fn json_map_bytes(values: &serde_json::Map<String, serde_json::Value>, meter: &mut ByteMeter) {
    // `serde_json::Map` is a BTreeMap unless preserve_order is
    // enabled. Charge a deliberately conservative node allowance so
    // either representation remains within this budget.
    meter.add_product(values.len(), 512);
    for (key, value) in values {
        meter.string_capacity(key);
        json_bytes(value, meter);
    }
}

/// Count a serializable opaque nested value without constructing a second
/// payload copy. Direct `String`/`Vec` owners still use capacities above;
/// this covers nested action/context representations whose concrete shape is
/// intentionally owned by `qos-actions`.
fn serialized_bytes<T: serde::Serialize>(value: &T, meter: &mut ByteMeter) {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .ok_or_else(|| std::io::Error::other("serialized size overflow"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    if serde_json::to_writer(&mut counter, value).is_ok() {
        meter.add(counter.0);
    } else {
        meter.0 = None;
    }
}

fn exported_action_bytes(action: &ExportedAction, meter: &mut ByteMeter) {
    meter.string_capacity(&action.resource);
    meter.string_capacity(&action.action);
    meter.string_capacity(&action.resource_kind);
    meter.string_capacity(&action.resource_label);
    meter.string_capacity(&action.action_label);
    if let Some(description) = &action.description {
        meter.string_capacity(description);
    }
    meter.string_capacity(&action.operation_kind);
    meter.string_capacity(&action.params_json);
    meter.string_capacity(&action.contract_json);
    if let ExportedInput::Text {
        prompt,
        placeholder,
        ..
    } = &action.input
    {
        meter.string_capacity(prompt);
        meter.string_capacity(placeholder);
    }
}

fn request_bytes(request: &CollabRequest) -> Option<usize> {
    let mut meter = ByteMeter(Some(std::mem::size_of::<CollabRequest>()));
    match request {
        CollabRequest::Hello {
            app_id,
            record,
            handle,
            ..
        } => {
            meter.string_capacity(app_id);
            if let Some(record) = record {
                meter.string_capacity(record);
            }
            if let Some(handle) = handle {
                meter.string_capacity(handle);
            }
        }
        CollabRequest::DocumentSnapshot { data } | CollabRequest::DocumentIncremental { data } => {
            meter.bytes(data);
        }
        CollabRequest::SignedOperation { envelope } => meter.bytes(&envelope.payload),
        CollabRequest::Presence { key, payload } => {
            serialized_bytes(key, &mut meter);
            presence_payload_bytes(payload, &mut meter);
        }
        CollabRequest::FetchScrollback { .. }
        | CollabRequest::OpenWorkspaceControl { .. }
        | CollabRequest::CloseWorkspaceControl { .. }
        | CollabRequest::PublishedInvocationStatus { .. }
        | CollabRequest::CancelPublishedInvocation { .. } => {}
        CollabRequest::PublishActions { app_id, actions } => {
            meter.string_capacity(app_id);
            meter.add_product(actions.capacity(), std::mem::size_of::<ExportedAction>());
            for action in actions {
                exported_action_bytes(action, &mut meter);
            }
        }
        CollabRequest::ReportContinuation {
            app_id, metadata, ..
        } => {
            meter.string_capacity(app_id);
            meter.bytes(metadata);
        }
        CollabRequest::ClearContinuation { app_id }
        | CollabRequest::LaunchPublishedApplication { app_id, .. } => meter.string_capacity(app_id),
        CollabRequest::WorkspaceControlInput { event, .. } => serialized_bytes(event, &mut meter),
        CollabRequest::CompleteInvocation { code, message, .. } => {
            meter.string_capacity(code);
            meter.string_capacity(message);
        }
        CollabRequest::UpdateInvocation { update, .. } => serialized_bytes(update, &mut meter),
        CollabRequest::RecordLocalEffect { address, update } => {
            meter.string_capacity(&address.resource);
            meter.string_capacity(&address.action);
            serialized_bytes(update, &mut meter);
        }
        CollabRequest::InvokePublishedAction {
            resource,
            action,
            arguments,
            confirmation,
            ..
        } => {
            meter.string_capacity(resource);
            meter.string_capacity(action);
            json_map_bytes(arguments, &mut meter);
            json_bytes(confirmation, &mut meter);
        }
        CollabRequest::ReportTaskRun { lease, update, .. } => {
            meter.string_capacity(lease);
            serialized_bytes(update, &mut meter);
        }
        CollabRequest::InvokeTaskAction {
            lease,
            resource,
            action,
            arguments,
            ..
        } => {
            meter.string_capacity(lease);
            meter.string_capacity(resource);
            meter.string_capacity(action);
            json_map_bytes(arguments, &mut meter);
        }
        CollabRequest::TaskRunCancellation { lease, .. } => meter.string_capacity(lease),
    }
    meter.finish()
}

fn command_bytes(command: &SdkCommand) -> Option<usize> {
    match command {
        SdkCommand::Primary(request) => request_bytes(request),
        SdkCommand::ToRoute(route, request) => {
            let mut meter = ByteMeter(request_bytes(request));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            meter.finish()
        }
        SdkCommand::Join(route) => {
            let mut meter = ByteMeter(Some(std::mem::size_of::<SdkCommand>()));
            meter.string_capacity(&route.app_id);
            if let Some(record) = &route.record {
                meter.string_capacity(record);
            }
            meter.finish()
        }
        SdkCommand::PrimaryReady(_) => Some(std::mem::size_of::<SdkCommand>()),
        // `ChannelGroup` intentionally hides its OpenMLS allocation graph,
        // so it cannot offer an exact capacity probe without widening the
        // crypto API. Installation is synchronous and requires `&mut self`,
        // which permits at most one group transfer at a time. Reserving the
        // entire command budget encodes that single-owner exception: no other
        // bridge payload may queue alongside it, while the group itself was
        // already allocated by the caller before entering this bridge.
        SdkCommand::InstallEncryption { .. } => Some(SDK_COMMAND_QUEUE_BYTES),
        SdkCommand::PublishActionDirectory { app_id, .. } => {
            std::mem::size_of::<SdkCommand>().checked_add(app_id.capacity())
        }
    }
}

fn presence_payload_bytes(payload: &PresencePayload, meter: &mut ByteMeter) {
    if let PresencePayload::Following {
        target: Some(target),
    } = payload
    {
        meter.string_capacity(target);
    }
    if let PresencePayload::Interaction(update) = payload {
        serialized_bytes(update.as_ref(), meter);
    }
}

fn workspace_control_event_bytes(
    event: &runtime_collab::WorkspaceControlEvent,
    meter: &mut ByteMeter,
) {
    match event {
        runtime_collab::WorkspaceControlEvent::Frame { encoded_h264, .. } => {
            meter.bytes(encoded_h264);
        }
        runtime_collab::WorkspaceControlEvent::Closed(
            runtime_collab::WorkspaceControlCloseReason::Denied(message),
        ) => {
            meter.string_capacity(message);
        }
        _ => {}
    }
}

fn broker_action_bytes(action: &BrokerActionDescriptor, meter: &mut ByteMeter) {
    meter.string_capacity(&action.resource);
    meter.string_capacity(&action.action);
    meter.string_capacity(&action.resource_kind);
    meter.string_capacity(&action.resource_label);
    meter.string_capacity(&action.action_label);
    if let Some(description) = &action.description {
        meter.string_capacity(description);
    }
    meter.add_product(
        action.params.capacity(),
        std::mem::size_of::<qos_actions::ActionParam>(),
    );
    for param in &action.params {
        serialized_bytes(param, meter);
    }
    serialized_bytes(&action.contract, meter);
    meter.string_capacity(&action.input.kind);
    meter.string_capacity(&action.input.prompt);
    meter.string_capacity(&action.input.placeholder);
}

fn context_event_bytes(
    collaborators: &[Collaborator],
    collaborator_capacity: usize,
    permissions: &[String],
    permission_capacity: usize,
    meter: &mut ByteMeter,
) {
    meter.add_product(collaborator_capacity, std::mem::size_of::<Collaborator>());
    for collaborator in collaborators {
        meter.string_capacity(&collaborator.id);
        meter.string_capacity(&collaborator.name);
    }
    meter.add_product(permission_capacity, std::mem::size_of::<String>());
    for permission in permissions {
        meter.string_capacity(permission);
    }
}

fn action_catalogue_bytes(
    surfaces: &[BrokerActionSurface],
    surface_capacity: usize,
    meter: &mut ByteMeter,
) {
    meter.add_product(surface_capacity, std::mem::size_of::<BrokerActionSurface>());
    for surface in surfaces {
        meter.string_capacity(&surface.app_id);
        meter.string_capacity(&surface.cluster_id);
        meter.string_capacity(&surface.record_id);
        meter.add_product(
            surface.actions.capacity(),
            std::mem::size_of::<BrokerActionDescriptor>(),
        );
        for action in &surface.actions {
            broker_action_bytes(action, meter);
        }
    }
}

fn scrollback_batch_bytes(
    ops: &[Box<SignedOp>],
    op_capacity: usize,
    decrypted_payloads: &[Option<Vec<u8>>],
    payload_capacity: usize,
    meter: &mut ByteMeter,
) {
    meter.add_product(op_capacity, std::mem::size_of::<Box<SignedOp>>());
    for op in ops {
        meter.bytes(&op.payload);
    }
    meter.add_product(payload_capacity, std::mem::size_of::<Option<Vec<u8>>>());
    for payload in decrypted_payloads.iter().flatten() {
        meter.bytes(payload);
    }
}

fn task_run_bytes(run: &runtime_collab::TaskRunEnvelope, meter: &mut ByteMeter) {
    meter.string_capacity(&run.task_id);
    meter.string_capacity(&run.title);
    meter.string_capacity(&run.lease);
    json_bytes(&run.action_context, meter);
}

fn event_bytes(event: &CollabEvent) -> Option<usize> {
    let mut meter = ByteMeter(Some(std::mem::size_of::<CollabEvent>()));
    match event {
        CollabEvent::Context {
            collaborators,
            permissions,
        } => context_event_bytes(
            collaborators,
            collaborators.capacity(),
            permissions,
            permissions.capacity(),
            &mut meter,
        ),
        CollabEvent::FollowChanged { peer_id } => {
            if let Some(peer_id) = peer_id {
                meter.string_capacity(peer_id);
            }
        }
        CollabEvent::RemoteCursorMoved { user_id, .. } => meter.string_capacity(user_id),
        CollabEvent::RemotePresenceUpdate {
            user_id,
            key,
            payload,
        } => {
            meter.string_capacity(user_id);
            serialized_bytes(key, &mut meter);
            presence_payload_bytes(payload, &mut meter);
        }
        CollabEvent::ActionArrived {
            type_id,
            payload,
            from_user_id,
        } => {
            meter.string_capacity(type_id);
            meter.bytes(payload);
            meter.string_capacity(from_user_id);
        }
        CollabEvent::Document {
            envelope,
            from_user_id,
            decrypted_payload,
        } => {
            meter.bytes(&envelope.payload);
            meter.string_capacity(from_user_id);
            if let Some(payload) = decrypted_payload {
                meter.bytes(payload);
            }
        }
        CollabEvent::ScrollbackBatch {
            ops,
            decrypted_payloads,
            ..
        } => scrollback_batch_bytes(
            ops,
            ops.capacity(),
            decrypted_payloads,
            decrypted_payloads.capacity(),
            &mut meter,
        ),
        CollabEvent::Invocation {
            resource,
            action,
            argument,
            actor,
            context,
            ..
        } => {
            meter.string_capacity(resource);
            meter.string_capacity(action);
            if let Some(argument) = argument {
                meter.string_capacity(argument);
            }
            meter.string_capacity(actor);
            if let Some(context) = context {
                serialized_bytes(context.as_ref(), &mut meter);
            }
        }
        CollabEvent::InvocationCancellation { .. } => {}
        CollabEvent::ActionCatalogue { surfaces } => {
            action_catalogue_bytes(surfaces, surfaces.capacity(), &mut meter);
        }
        CollabEvent::PublishedInvocationResult { result, .. }
        | CollabEvent::TaskActionInvocationResult { result, .. } => match result {
            Ok((_, update)) => serialized_bytes(update, &mut meter),
            Err(error) => meter.string_capacity(error),
        },
        CollabEvent::TaskRun(run) => task_run_bytes(run, &mut meter),
        CollabEvent::TaskRunCancellation { result, .. } => {
            if let Err(error) = result {
                meter.string_capacity(error);
            }
        }
        CollabEvent::WorkspaceControl { event, .. } => {
            workspace_control_event_bytes(event, &mut meter);
        }
    }
    meter.finish()
}

/// Shared failure latch. The UI reads it through `is_connected()` even when
/// overload is detected by a subscription task on the shovel thread.
#[derive(Clone)]
struct BridgeState {
    healthy: Arc<AtomicBool>,
    command_bytes: ByteBudget,
    event_bytes: ByteBudget,
}

/// Lock-free aggregate reservation for heap payload retained by one queue.
/// A reservation moves with its queued value and releases automatically when
/// the receiver consumes or drops that value.
#[derive(Clone)]
struct ByteBudget(Arc<AtomicUsize>);

struct ByteReservation {
    budget: ByteBudget,
    bytes: usize,
}

impl ByteBudget {
    fn new() -> Self {
        Self(Arc::new(AtomicUsize::new(0)))
    }

    fn reserve(&self, bytes: usize, limit: usize) -> Option<ByteReservation> {
        let mut used = self.0.load(Ordering::Acquire);
        loop {
            let next = used.checked_add(bytes)?;
            if next > limit {
                return None;
            }
            match self
                .0
                .compare_exchange_weak(used, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    return Some(ByteReservation {
                        budget: self.clone(),
                        bytes,
                    });
                }
                Err(observed) => used = observed,
            }
        }
    }
}

impl Drop for ByteReservation {
    fn drop(&mut self) {
        self.budget.0.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

struct QueuedSdkCommand {
    command: SdkCommand,
    _reservation: ByteReservation,
}

struct QueuedCollabEvent {
    event: CollabEvent,
    _reservation: ByteReservation,
}

impl BridgeState {
    fn new() -> Self {
        Self {
            healthy: Arc::new(AtomicBool::new(true)),
            command_bytes: ByteBudget::new(),
            event_bytes: ByteBudget::new(),
        }
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn fail_closed(&self, direction: &'static str) {
        if self.healthy.swap(false, Ordering::AcqRel) {
            tracing::error!(
                direction,
                "collab: bounded bridge overloaded or disconnected; disabling collaboration"
            );
        }
    }
}

/// Non-blocking UI enqueue. Presence may be dropped when the finite queue is
/// full; a durable command makes the whole bridge unavailable instead.
fn enqueue_sdk_command(
    tx: &tokio_mpsc::Sender<QueuedSdkCommand>,
    command: SdkCommand,
    state: &BridgeState,
) -> bool {
    if !state.is_healthy() {
        return false;
    }
    let transient = command.is_transient_presence();
    let Some(reservation) = state.command_bytes.reserve(
        command_bytes(&command).unwrap_or(usize::MAX),
        SDK_COMMAND_QUEUE_BYTES,
    ) else {
        if !transient {
            state.fail_closed("outbound command byte budget");
        }
        return false;
    };
    match tx.try_send(QueuedSdkCommand {
        command,
        _reservation: reservation,
    }) {
        Ok(()) => true,
        Err(tokio_mpsc::error::TrySendError::Full(_)) if transient => false,
        Err(_) => {
            state.fail_closed("outbound command queue");
            false
        }
    }
}

/// Bounded inbound half with the same split between durable events and
/// explicitly lossy presence. Stream tasks stop after a durable failure, so
/// they cannot keep pulling broker frames after the connection is failed.
#[derive(Clone)]
struct InboundBridge {
    tx: tokio_mpsc::Sender<QueuedCollabEvent>,
    state: BridgeState,
}

impl InboundBridge {
    fn send_durable(&self, event: CollabEvent) -> bool {
        let Some(reservation) = self.state.event_bytes.reserve(
            event_bytes(&event).unwrap_or(usize::MAX),
            SDK_EVENT_QUEUE_BYTES,
        ) else {
            self.state.fail_closed("inbound event byte budget");
            return false;
        };
        if self
            .tx
            .try_send(QueuedCollabEvent {
                event,
                _reservation: reservation,
            })
            .is_ok()
        {
            true
        } else {
            self.state.fail_closed("inbound event queue");
            false
        }
    }

    /// Returns false only when the receiver has gone away. A full queue is a
    /// permitted transient-presence drop and does not terminate the stream.
    fn send_presence(&self, event: CollabEvent) -> bool {
        let Some(reservation) = self.state.event_bytes.reserve(
            event_bytes(&event).unwrap_or(usize::MAX),
            SDK_EVENT_QUEUE_BYTES,
        ) else {
            return true;
        };
        match self.tx.try_send(QueuedCollabEvent {
            event,
            _reservation: reservation,
        }) {
            Ok(()) | Err(tokio_mpsc::error::TrySendError::Full(_)) => true,
            Err(tokio_mpsc::error::TrySendError::Closed(_)) => {
                self.state.fail_closed("inbound presence queue");
                false
            }
        }
    }
}

/// How the shovel thread reaches the broker. `Env` is production: the
/// launcher passed `QOS_SVC_COLLAB_FD`. `Fd` is an already-connected typed
/// endpoint, used by tests and headless diagnostics without environment races.
enum SdkConnectSource {
    Env,
    Fd(RawFd),
}

impl SdkConnectSource {
    fn build_client(self) -> Result<runtime_collab::CollabClient, runtime_portal::NotAvailable> {
        match self {
            SdkConnectSource::Env => runtime_collab::try_connect(),
            SdkConnectSource::Fd(fd) => {
                use capnp_rpc::rpc_twoparty_capnp;
                let unix = runtime_portal::UnixCapnpClient::from_connected_fd(
                    fd,
                    rpc_twoparty_capnp::Side::Client,
                )?;
                let raw: runtime_collab::CollabServiceClient = unix.bootstrap();
                Ok(runtime_collab::CollabClient::from_raw(raw))
            }
        }
    }
}

/// Build the identity presentation for a launcher-bound collaboration join.
///
/// Build the certified device presentation used for a launcher-bound join.
/// A launcher identity without a device certificate cannot enter the
/// collaboration session.
struct SdkJoinIdentity {
    presentation: Vec<u8>,
    /// Requested broker principal kind. The broker only accepts `agent`
    /// when the named agent is admitted and this certified user may operate
    /// it; ordinary applications always leave this as `person`.
    participant_kind: &'static str,
    /// Retained on the shovel thread so document signing stays off the UI
    /// thread and the workspace broker connection is not consumed at join.
    signer: SdkDocumentSigner,
}

/// Certified identity for a client connecting to an explicit collaboration
/// endpoint rather than through a launcher-inherited identity service.
#[derive(Clone)]
pub struct CertifiedEndpointIdentity {
    presentation: Vec<u8>,
    device_key: ed25519_dalek::SigningKey,
}

impl CertifiedEndpointIdentity {
    pub fn new(presentation: Vec<u8>, device_key: ed25519_dalek::SigningKey) -> Self {
        Self {
            presentation,
            device_key,
        }
    }
}

impl std::fmt::Debug for CertifiedEndpointIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertifiedEndpointIdentity")
            .field(
                "presentation",
                &format_args!("{} bytes", self.presentation.len()),
            )
            .field("device_key", &"<redacted>")
            .finish()
    }
}

enum SdkDocumentSigner {
    #[cfg(unix)]
    Certified(Box<runtime_identity::SigningIdentity>),
    /// Explicit-FD harnesses have no launcher identity service. Their
    /// in-process broker accepts this ephemeral signer; a production broker
    /// still rejects the uncertified join before any op reaches policy.
    Harness(Box<ed25519_dalek::SigningKey>),
}

fn harness_document_signer() -> SdkDocumentSigner {
    SdkDocumentSigner::Harness(Box::new(ed25519_dalek::SigningKey::generate(
        &mut rand_core::OsRng,
    )))
}

#[cfg(unix)]
fn sdk_join_identity(cluster_id: Uuid) -> Result<SdkJoinIdentity, runtime_portal::NotAvailable> {
    let identity = runtime_identity::load()?;
    let device_certificate = identity.device_certificate().ok_or_else(|| {
        runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "workspace identity has no certified device",
        ))
    })?;

    let workspace_id = cluster_id.to_string();
    let proof = identity.sign_collaboration_join(&workspace_id)?;
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id,
        device_certificate: device_certificate.to_vec(),
        proof: proof.to_vec(),
    }
    .encode()
    .map_err(|error| {
        runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("encoding certified collaboration join failed: {error}"),
        ))
    })?;
    Ok(SdkJoinIdentity {
        presentation,
        participant_kind: "person",
        signer: SdkDocumentSigner::Certified(Box::new(identity)),
    })
}

#[cfg(not(unix))]
fn sdk_join_identity(_cluster_id: Uuid) -> Result<SdkJoinIdentity, runtime_portal::NotAvailable> {
    Err(runtime_portal::NotAvailable::ServiceFdMissing)
}

pub struct CollaborationManager {
    backend: Backend,
    /// When `Some`, [`Self::send_signed_op`] mints a signed envelope and
    /// dispatches [`CollabRequest::SignedOperation`]. This explicit
    /// policy-envelope API is separate from document mutations, which the SDK
    /// signs with the certified launcher identity.
    identity: Option<SigningIdentity>,
}

impl CollaborationManager {
    /// Connect to the launcher-inherited typed collaboration service and
    /// announce an explicit route. Headless participants use this instead of
    /// relying on process-wide `QUICK_APP_ID` / `QOS_WORKSPACE_ID` variables.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the launcher did not
    /// hand down a collab service connection.
    pub fn for_route(
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Env,
            app_id.into(),
            cluster_id,
            record,
            handle,
            None,
            "person",
        )
    }

    /// Explicit typed endpoint for in-process and headless transport
    /// harnesses. Takes ownership of `fd`. It uses an ephemeral signer; a
    /// production broker rejects the uncertified join, so this cannot bypass
    /// workspace admission.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when `fd` does not bootstrap
    /// a working collab service connection.
    pub fn for_route_fd(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            handle,
            None,
            "person",
        )
    }

    /// Connect to an explicit endpoint with a certified device presentation.
    /// This is the headless equivalent of the launcher-provided identity path.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when `fd` cannot bootstrap a
    /// collaboration service connection.
    pub fn for_route_fd_with_identity(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
        identity: CertifiedEndpointIdentity,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            handle,
            Some(SdkJoinIdentity {
                presentation: identity.presentation,
                participant_kind: "person",
                signer: SdkDocumentSigner::Harness(Box::new(identity.device_key)),
            }),
            "person",
        )
    }

    fn for_route_from(
        source: SdkConnectSource,
        app_id: String,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
        endpoint_identity: Option<SdkJoinIdentity>,
        participant_kind: &'static str,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        let mut identity = match endpoint_identity {
            Some(identity) => identity,
            None if matches!(&source, SdkConnectSource::Env) => sdk_join_identity(cluster_id)?,
            None => SdkJoinIdentity {
                presentation: Vec::new(),
                participant_kind: "person",
                signer: harness_document_signer(),
            },
        };
        identity.participant_kind = participant_kind;
        let sdk = Self::spawn_sdk(source, identity)?;
        let mut manager = Self {
            backend: Backend::Sdk(sdk),
            identity: None,
        };
        manager.send(CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            handle,
        });
        manager.await_primary_route()?;
        Ok(manager)
    }

    /// Wait until the shovel has processed the initial `Hello`, admitted its
    /// primary route, and queued the route's initial action catalogue. Without
    /// this barrier a caller can publish transient presence immediately after
    /// construction, before the broker knows where to deliver it.
    fn await_primary_route(&mut self) -> Result<(), runtime_portal::NotAvailable> {
        let Backend::Sdk(channel) = &mut self.backend else {
            return Err(runtime_portal::NotAvailable::ServiceFdMissing);
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        if !enqueue_sdk_command(
            &channel.outbound_tx,
            SdkCommand::PrimaryReady(ready_tx),
            &channel.state,
        ) {
            return Err(runtime_portal::NotAvailable::ConnectionFailed(
                std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "collaboration setup thread is unavailable or overloaded",
                ),
            ));
        }
        match ready_rx.recv_timeout(SHOVEL_INIT_TIMEOUT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(runtime_portal::NotAvailable::ConnectionFailed(
                std::io::Error::new(std::io::ErrorKind::ConnectionRefused, error),
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(
                runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "collaboration primary route did not become ready in time",
                )),
            ),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(
                runtime_portal::NotAvailable::ConnectionFailed(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "collaboration setup thread ended before reporting route readiness",
                )),
            ),
        }
    }

    /// Connect a headless participant as a first-class workspace agent.
    /// Its friendly id is both the visible handle and the principal whose
    /// scoped capabilities the broker evaluates.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the collaboration
    /// service or certified join identity is unavailable.
    pub fn for_agent_route(
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        agent_id: impl Into<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Env,
            app_id.into(),
            cluster_id,
            record,
            Some(agent_id.into()),
            None,
            "agent",
        )
    }

    /// Explicit-socket counterpart to [`Self::for_agent_route`]. Production
    /// still requires a certified endpoint identity; this method merely
    /// preserves the agent principal kind for diagnostic transports.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the socket cannot be
    /// joined or the route identity cannot be established.
    pub fn for_agent_route_fd(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        agent_id: impl Into<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            Some(agent_id.into()),
            None,
            "agent",
        )
    }

    /// Certified explicit-socket agent connection used by out-of-process
    /// runtimes that do not inherit the launcher's service descriptor.
    ///
    /// # Errors
    ///
    /// Returns [`runtime_portal::NotAvailable`] when the socket or certified
    /// route join fails.
    pub fn for_agent_route_fd_with_identity(
        fd: RawFd,
        app_id: impl Into<String>,
        cluster_id: Uuid,
        record: Option<String>,
        agent_id: impl Into<String>,
        identity: CertifiedEndpointIdentity,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        Self::for_route_from(
            SdkConnectSource::Fd(fd),
            app_id.into(),
            cluster_id,
            record,
            Some(agent_id.into()),
            Some(SdkJoinIdentity {
                presentation: identity.presentation,
                participant_kind: "agent",
                signer: SdkDocumentSigner::Harness(Box::new(identity.device_key)),
            }),
            "agent",
        )
    }

    /// Connects through `runtime_collab::try_connect()`, entering SDK mode on
    /// `Ok` and Disabled mode on `Err`.
    ///
    /// Both modes auto-send `Hello` announcing **no record** (one document
    /// per `(app, room)`); use [`Self::for_record`] for apps that have
    /// adopted records.
    pub fn new() -> Self {
        Self::new_scoped(
            continuation_instance_env().map(|instance| format!("continuation:{instance}")),
        )
    }
}

/// Launcher-issued instance identity for an app window. It deliberately rides
/// in the existing record component of the authenticated CRDT route so the
/// broker can retain multiple Browser windows in one workspace without
/// conflating their continuation metadata.
fn continuation_instance_env() -> Option<String> {
    let value = std::env::var("QOS_CONTINUITY_INSTANCE").ok()?;
    let value = value.trim();
    (!value.is_empty() && value.len() <= 256 && !value.contains(['/', '\\']))
        .then(|| value.to_owned())
}

impl Default for CollaborationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CollaborationManager {
    /// A manager scoped to **one record** — one document, one change graph,
    /// one channel — so two people editing different files in one room
    /// don't get handed each other's state. A process with several records
    /// open holds several managers; `ui::App` swaps its manager on checkout
    /// change.
    pub fn for_record(record: &crate::record::RecordId) -> Self {
        Self::new_scoped(Some(record.as_str().to_string()))
    }

    fn new_scoped(record: Option<String>) -> Self {
        let (app_id, cluster_id) = auto_hello_env();
        let identity = match sdk_join_identity(cluster_id) {
            Ok(identity) => identity,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "collab: workspace join identity unavailable; disabling SDK collaboration"
                );
                return Self {
                    backend: Backend::Disabled,
                    identity: None,
                };
            }
        };

        match Self::spawn_sdk(SdkConnectSource::Env, identity) {
            Ok(sdk) => {
                let mut manager = Self {
                    backend: Backend::Sdk(sdk),
                    identity: None,
                };
                manager.send(CollabRequest::Hello {
                    app_id,
                    cluster_id,
                    record,
                    handle: presence_handle_env(),
                });
                if let Err(error) = manager.await_primary_route() {
                    tracing::warn!(
                        ?error,
                        "collab: primary route unavailable; disabling SDK collaboration"
                    );
                    return Self {
                        backend: Backend::Disabled,
                        identity: None,
                    };
                }
                manager
            }
            Err(_) => Self {
                backend: Backend::Disabled,
                identity: None,
            },
        }
    }

    /// Test-only: build a manager talking to an SDK broker on the given
    /// already-connected client fd, bypassing env-var lookup so two
    /// tests in one process don't race on `QOS_SVC_COLLAB_FD`.
    #[cfg(test)]
    fn from_sdk_fd_for_test(fd: RawFd, app_id: &str, cluster_id: Uuid) -> Self {
        Self::from_sdk_fd_for_test_with_identity(fd, app_id, cluster_id, Vec::new())
    }

    /// Like [`Self::from_sdk_fd_for_test`] but stamps the `join_channel`
    /// call with a caller-supplied identity blob.
    #[cfg(test)]
    fn from_sdk_fd_for_test_with_identity(
        fd: RawFd,
        app_id: &str,
        cluster_id: Uuid,
        identity_bytes: Vec<u8>,
    ) -> Self {
        let sdk = Self::spawn_sdk(
            SdkConnectSource::Fd(fd),
            SdkJoinIdentity {
                presentation: identity_bytes,
                participant_kind: "person",
                signer: harness_document_signer(),
            },
        )
        .expect("spawn_sdk from Fd source must succeed in tests");
        let mut manager = Self {
            backend: Backend::Sdk(sdk),
            identity: None,
        };
        manager.send(CollabRequest::Hello {
            app_id: app_id.to_owned(),
            cluster_id,
            record: None,
            handle: presence_handle_env(),
        });
        manager
            .await_primary_route()
            .expect("test collaboration primary route becomes ready");
        manager
    }

    /// Stand up the shovel thread + its tokio `current_thread` runtime.
    /// Runs `try_connect` **inside** the thread's `LocalSet` because
    /// capnp-rpc futures are `!Send` and can't cross a thread boundary.
    /// The result surfaces via [`InitStatus`] on a one-shot mpsc so the
    /// caller picks Disabled vs Sdk before returning, instead of an app
    /// that only ever `poll`s silently running against a dead broker.
    fn spawn_sdk(
        source: SdkConnectSource,
        identity: SdkJoinIdentity,
    ) -> Result<SdkChannel, runtime_portal::NotAvailable> {
        let (outbound_tx, outbound_rx) = tokio_mpsc::channel::<QueuedSdkCommand>(SDK_COMMAND_QUEUE);
        let (inbound_tx, inbound_rx) = tokio_mpsc::channel::<QueuedCollabEvent>(SDK_EVENT_QUEUE);
        let state = BridgeState::new();
        let inbound = InboundBridge {
            tx: inbound_tx,
            state: state.clone(),
        };
        let (action_directory_tx, action_directory_rx) =
            mpsc::sync_channel::<ActionDirectoryRequest>(ACTION_DIRECTORY_REQUEST_QUEUE);
        let (init_tx, init_rx) = mpsc::sync_channel::<InitStatus>(1);

        // A thread we can't start is a service we can't reach — map to
        // NotAvailable rather than panicking; an app must open even without collab.
        let handle = match thread::Builder::new()
            .name("ui-collab-shovel".into())
            .spawn(move || run_shovel(source, identity, outbound_rx, inbound, init_tx))
        {
            Ok(handle) => handle,
            Err(e) => return Err(runtime_portal::NotAvailable::ConnectionFailed(e)),
        };

        // Must not wait forever or panic — this runs before the app's first
        // frame. A wedged connect costs a timeout and no collaboration, never
        // an app that won't open.
        let status = match init_rx.recv_timeout(SHOVEL_INIT_TIMEOUT) {
            Ok(status) => status,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(
                    "collab: the connect path did not report within {SHOVEL_INIT_TIMEOUT:?}; \
                     continuing without collaboration"
                );
                return Err(runtime_portal::NotAvailable::ConnectionFailed(
                    std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "collab connect did not report in time",
                    ),
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                tracing::warn!(
                    "collab: the connect path ended without reporting; \
                     continuing without collaboration"
                );
                return Err(runtime_portal::NotAvailable::ConnectionFailed(
                    std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "collab connect ended without reporting",
                    ),
                ));
            }
        };

        match status {
            InitStatus::Ready => Ok(SdkChannel {
                outbound_tx,
                inbound_rx,
                state,
                action_directory_rx,
                action_directory_tx,
                action_directory_published: false,
                _thread: Some(handle),
            }),
            InitStatus::Failed(e) => {
                // Let the (empty-body) thread exit cleanly.
                let _ = handle.join();
                Err(e)
            }
        }
    }

    /// True when connected to a live broker — used by the
    /// event loop to keep ticking so the stream drains promptly.
    pub fn is_connected(&self) -> bool {
        matches!(&self.backend, Backend::Sdk(channel) if channel.state.is_healthy())
    }

    /// Bind a signing identity, builder-style for chaining at boot.
    /// Replaces any previous identity.
    #[must_use]
    pub fn with_identity(mut self, identity: SigningIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Replace the bound identity in place, e.g. when the user switches
    /// identities in the chrome's identity picker without tearing down the manager.
    pub fn set_identity(&mut self, identity: SigningIdentity) {
        self.identity = Some(identity);
    }

    pub fn has_identity(&self) -> bool {
        self.identity.is_some()
    }

    /// Sign and send a CRDT mutation envelope. Best-effort like
    /// [`Self::send`]: an unsent frame isn't retried, but the next live
    /// edit republishes state, bounding the loss to one round-trip.
    ///
    /// # Errors
    ///
    /// Returns [`SignedSendError::NoIdentity`] when no signing identity is
    /// bound, [`SignedSendError::SequenceExhausted`] before a sequence could
    /// be reused, or [`SignedSendError::Canonical`] when `payload` fails to
    /// canonicalize for signing.
    pub fn send_signed_op(
        &mut self,
        payload: Vec<u8>,
        artifact: collab_types::ArtifactId,
        op_class: collab_types::OpClass,
    ) -> Result<(), SignedSendError> {
        let identity = self.identity.as_mut().ok_or(SignedSendError::NoIdentity)?;
        let envelope = identity.sign_op(payload, artifact, op_class)?;
        self.send(CollabRequest::SignedOperation {
            envelope: Box::new(envelope),
        });
        Ok(())
    }

    /// Fire-and-forget and non-blocking on every backend. Presence may be
    /// shed under saturation; any durable enqueue failure visibly disconnects
    /// the manager, so callers never continue under the illusion it was sent.
    pub fn send(&mut self, req: CollabRequest) {
        if let Backend::Sdk(ch) = &self.backend {
            let _ = enqueue_sdk_command(&ch.outbound_tx, SdkCommand::Primary(req), &ch.state);
        }
    }

    /// Register the UI-owned pull directory on the primary broker route.
    /// Re-registering is harmless: the broker atomically replaces the old
    /// capability for this live connection.
    pub fn publish_action_directory(&mut self, app_id: impl Into<String>) {
        if let Backend::Sdk(ch) = &mut self.backend {
            if ch.action_directory_published {
                return;
            }
            if enqueue_sdk_command(
                &ch.outbound_tx,
                SdkCommand::PublishActionDirectory {
                    app_id: app_id.into(),
                    requests: ch.action_directory_tx.clone(),
                },
                &ch.state,
            ) {
                ch.action_directory_published = true;
            }
        }
    }

    /// Drain requests issued by the shovel-side `ActionDirectory` server. This
    /// must be called by [`crate::app::App`] on its owner thread.
    pub(crate) fn take_action_directory_requests(&mut self) -> Vec<ActionDirectoryRequest> {
        let Backend::Sdk(ch) = &mut self.backend else {
            return Vec::new();
        };
        let mut requests = Vec::new();
        while let Ok(request) = ch.action_directory_rx.try_recv() {
            requests.push(request);
        }
        requests
    }

    /// Join and subscribe to an additional route without publishing traffic.
    /// Repeated joins of the same route are idempotent.
    pub fn join_route(&mut self, route: RouteSpec) {
        if let Backend::Sdk(ch) = &mut self.backend {
            let _ = enqueue_sdk_command(&ch.outbound_tx, SdkCommand::Join(route), &ch.state);
        }
    }

    /// Install an already-provisioned MLS endpoint for `route`.
    ///
    /// From this point the route is fail-closed: locally generated document
    /// mutations are encrypted and encrypted inbound envelopes are decrypted
    /// before becoming [`CollabEvent::Document`].  The channel id must be the
    /// deterministic id for the exact app/workspace/record route.  Group
    /// provisioning and durable secret storage intentionally remain outside
    /// the UI process; callers should obtain the group from the workspace
    /// identity service and must reinstall it after a process restart.
    ///
    /// # Errors
    ///
    /// Returns an error for a route/channel mismatch, an unavailable or
    /// stopped collaboration worker, or a route that cannot be joined.
    pub fn install_document_encryption(
        &mut self,
        route: RouteSpec,
        group: channel_crypto::ChannelGroup,
    ) -> Result<(), String> {
        let record = route.record.as_deref().unwrap_or("");
        let expected =
            collab_types::document_channel_id(&route.app_id, &route.cluster_id.to_string(), record);
        if group.channel_id() != expected {
            return Err("MLS group does not belong to the requested document route".into());
        }
        let Backend::Sdk(ch) = &mut self.backend else {
            return Err("collaboration broker is unavailable".into());
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        if !enqueue_sdk_command(
            &ch.outbound_tx,
            SdkCommand::InstallEncryption {
                spec: route,
                group: Box::new(group),
                reply: reply_tx,
            },
            &ch.state,
        ) {
            return Err("collaboration worker is unavailable or overloaded".to_string());
        }
        reply_rx
            .recv()
            .map_err(|_| "collaboration worker stopped".to_string())?
    }

    /// Publish on an explicitly named additional route. The route is joined
    /// lazily and retained, and its inbound streams feed this manager's normal
    /// [`Self::poll`] queue.
    pub fn send_to_route(&mut self, route: RouteSpec, req: CollabRequest) {
        if matches!(req, CollabRequest::Hello { .. }) {
            tracing::warn!("collab: send_to_route ignores Hello; use join_route");
            return;
        }
        if let Backend::Sdk(ch) = &mut self.backend {
            let _ =
                enqueue_sdk_command(&ch.outbound_tx, SdkCommand::ToRoute(route, req), &ch.state);
        }
    }

    /// Drain every event pending on the current backend. Non-blocking.
    pub fn poll(&mut self) -> Vec<CollabEvent> {
        match &mut self.backend {
            Backend::Disabled => Vec::new(),
            Backend::Sdk(ch) => {
                let mut events = Vec::new();
                while let Ok(evt) = ch.inbound_rx.try_recv() {
                    events.push(evt.event);
                }
                events
            }
        }
    }
}

/// The presence handle announced on `Hello`. `QOS_PRESENCE_HANDLE` wins,
/// then `QOS_USER_ID`; absent both, the broker's synthetic name.
pub fn presence_handle_env() -> Option<String> {
    ["QOS_PRESENCE_HANDLE", "QOS_USER_ID"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
}

/// The routing key one connection names on the capnp transport.
///
/// An app's traffic is addressed by `(app_id, cluster_id, record_id)`,
/// but capnp's `joinChannel(channelId, identity)` carries a single
/// string, so the triple travels *inside* the channel id. This pair of
/// functions is the only place the encoding is written down, so the two
/// halves cannot drift apart.
///
/// # Shape
///
/// ```text
/// <cluster>[#app:<app_id>][#record:<record_id>]
/// ```
///
/// A uuid contains no `#`. `#record:` is stripped first and takes
/// everything after it, so a `#`-containing record id (a file path may
/// have one) stays intact. Both suffixes are optional, so a missing
/// `#app:` yields an empty app id rather than failing to parse — needed
/// for the earlier two-field keys chrome traffic still rides.
///
/// Without the app id in this key, every packaged app would publish
/// deltas cluster-scoped — one app's changes delivered to every app in
/// the room.
pub mod channel_key {
    /// Encode a route triple into one channel id.
    pub fn format(app_id: &str, cluster_id: &str, record_id: &str) -> String {
        let mut key = String::from(cluster_id);
        if !app_id.is_empty() {
            key.push_str("#app:");
            key.push_str(app_id);
        }
        if !record_id.is_empty() {
            key.push_str("#record:");
            key.push_str(record_id);
        }
        key
    }

    /// Decode a channel id into `(app_id, cluster_id, record_id)`. Total:
    /// every string decodes, and one naming no marker is a bare cluster.
    pub fn parse(key: &str) -> (String, String, String) {
        let (head, record_id) = match key.split_once("#record:") {
            Some((head, record)) => (head, record.to_string()),
            None => (key, String::new()),
        };
        let (cluster, app_id) = match head.split_once("#app:") {
            Some((cluster, app)) => (cluster, app.to_string()),
            None => (head, String::new()),
        };
        (app_id, cluster.to_string(), record_id)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn round_trips_every_combination_of_present_fields() {
            for (app, cluster, record) in [
                ("", "c", ""),
                ("app", "c", ""),
                ("", "c", "rec"),
                ("app", "c", "rec"),
            ] {
                let key = format(app, cluster, record);
                assert_eq!(
                    parse(&key),
                    (app.to_string(), cluster.to_string(), record.to_string()),
                    "key {key:?} did not round-trip",
                );
            }
        }

        #[test]
        fn keys_written_before_the_app_segment_still_parse() {
            // Both must decode to a cluster-scoped address rather than
            // nothing, or an app that hasn't been rebuilt stops routing.
            assert_eq!(
                parse("cluster-1"),
                (String::new(), "cluster-1".into(), String::new()),
            );
            assert_eq!(
                parse("cluster-1#record:/notes/a.md"),
                (String::new(), "cluster-1".into(), "/notes/a.md".into()),
            );
        }

        #[test]
        fn a_record_id_containing_a_hash_survives() {
            // Record ids are file paths and a path may contain `#`.
            // Stripping `#record:` first and keeping the whole tail is
            // what makes that safe.
            let key = format("app", "c", "/notes/draft#2.md");
            assert_eq!(
                parse(&key),
                ("app".into(), "c".into(), "/notes/draft#2.md".into()),
            );
        }
    }
}

/// The `(app_id, cluster_id)` pair this process announces in
/// [`CollabRequest::Hello`] — the broker delivers a CRDT delta only to
/// connections announcing the same pair.
///
/// # Precedence (each half resolves independently)
///
/// 1. `QOS_PACKAGE_ID` / `QOS_WORKSPACE_ID` — launcher-set for every
///    compositor-started app (reverse-DNS app id, opened workspace).
/// 2. `QUICK_APP_ID` / `QUICK_CLUSTER_ID` — standalone-dev override,
///    ranked below `QOS_PACKAGE_ID` so a dev identity never fragments
///    routing under a real session.
/// 3. A fresh random app id + the nil cluster — fails closed: nothing
///    else announces the same id, so nothing is ever delivered here.
///
/// `QOS_WORKSPACE_ID` counts as the cluster only when it parses as a
/// uuid — it's also read elsewhere as a bare "am I on qOS?" flag, so an
/// unroutable value falls through instead of being rejected.
fn auto_hello_env() -> (String, Uuid) {
    auto_hello_env_uncached()
}

/// The person sitting at this machine. Actions are stamped with this so
/// undo can tell collaborators apart (see
/// [`crate::state::State::undo_as`]): the id must be **stable** across a
/// document's life and **distinct** between people, or one person's
/// Ctrl+Z reverts another's work.
///
/// Mirrors the shell-side `local_user_id` ladder rung for rung (plus a
/// sandboxed-app rung) so a compositor-granted capability and an
/// app-stamped undo entry always name the same subject:
///
/// 1. `QOS_USER_ID` — explicit override; the shell reads it first too.
/// 2. The launcher-bound runtime identity (`QOS_USER_IDENTITY`) — the
///    rung a packaged app actually lands on, since `/var/lib/qos` is
///    outside its sandbox.
/// 3. `$XDG_CONFIG_HOME/qos/identity/primary.json`, for the shell's
///    own unsandboxed in-process apps.
/// 4. `/var/lib/qos/user-identity/pubkey.bin`.
/// 5. The OS account name, then a constant.
///
/// Not memoised — see [`auto_hello_env_uncached`]: a process hosting
/// more than one identity must not be frozen into the first one.
pub fn local_user_id() -> crate::permissions::UserId {
    crate::permissions::UserId(resolve_local_user_id())
}

fn resolve_local_user_id() -> String {
    if let Some(v) = non_empty_env("QOS_USER_ID") {
        return v;
    }
    #[cfg(unix)]
    {
        if let Some(id) = runtime_identity::load().ok().or_else(|| {
            config_identity_path().and_then(|p| runtime_identity::load_from_path(p).ok())
        }) {
            return person_id_from_key(id.group_public_key());
        }
    }
    if let Ok(bytes) = std::fs::read("/var/lib/qos/user-identity/pubkey.bin")
        && let Ok(key) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        return person_id_from_key(key);
    }
    non_empty_env("USER").unwrap_or_else(|| "local".to_string())
}

/// `$XDG_CONFIG_HOME/qos/identity/primary.json`, else
/// `$HOME/.config/qos/identity/primary.json`.
fn config_identity_path() -> Option<std::path::PathBuf> {
    let base = match non_empty_env("XDG_CONFIG_HOME") {
        Some(v) => std::path::PathBuf::from(v),
        None => std::path::PathBuf::from(non_empty_env("HOME")?).join(".config"),
    };
    let p = base.join("qos").join("identity").join("primary.json");
    p.is_file().then_some(p)
}

/// A public key rendered as a person, in the one canonical form — so this
/// string and the compositor's string for the same key are the same string.
fn person_id_from_key(key: [u8; 32]) -> String {
    // `person_from_key` constructs `Person` unconditionally, so the else
    // branch cannot run. It recomputes the same canonical lowercase-hex form
    // directly rather than panic on a branch that should never be reached.
    if let crate::permissions::Principal::Person(id) =
        crate::permissions::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(key),
        )
    {
        id
    } else {
        use std::fmt::Write as _;
        let mut s = String::with_capacity(64);
        for b in &key {
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}

/// The application this process is, per `QOS_PACKAGE_ID` (falling back
/// to `QUICK_APP_ID` for dev). Read from the same place the channel key
/// is built, so what an app publishes can't disagree with its connection.
pub fn local_app_id() -> String {
    auto_hello_env().0
}

/// The room this process collaborates in. Exposed so presence scopes
/// from the same source routing uses, rather than a derivation that
/// could drift.
///
/// `Uuid::nil()` means no workspace — the broker rejects it
/// deliberately, so callers must treat it as "no room", not a room.
pub fn local_cluster_id() -> Uuid {
    auto_hello_env().1
}

fn auto_hello_env_uncached() -> (String, Uuid) {
    // Only the random fallback id is memoised — caching the env-derived
    // answer would freeze every later reader into the first caller's
    // room, since env is the routing truth (a property of the launch, not
    // the process image) and can differ per manager in one process (every
    // collaboration test does this).
    static FALLBACK_APP_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    let app_id = non_empty_env("QOS_PACKAGE_ID")
        .or_else(|| non_empty_env("QUICK_APP_ID"))
        .unwrap_or_else(|| {
            FALLBACK_APP_ID
                .get_or_init(|| Uuid::new_v4().to_string())
                .clone()
        });

    let cluster_id = non_empty_env("QOS_WORKSPACE_ID")
        .and_then(|s| {
            if let Ok(id) = Uuid::parse_str(&s) {
                Some(id)
            } else {
                tracing::debug!(
                    "QOS_WORKSPACE_ID={s:?} is not a uuid; \
                 falling back to QUICK_CLUSTER_ID for collaboration routing"
                );
                None
            }
        })
        .or_else(|| non_empty_env("QUICK_CLUSTER_ID").and_then(|s| Uuid::parse_str(&s).ok()))
        .unwrap_or_else(Uuid::nil);

    (app_id, cluster_id)
}

/// `std::env::var` with empty treated as unset — an explicit `""` is
/// the shape an unset forwarded shell variable produces, and taking it
/// literally would put every such app in the same (empty-id) room.
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// One-shot init result from the shovel thread's startup, letting
/// `spawn_sdk` decide immediately between `SdkChannel` and `Disabled`.
enum InitStatus {
    Ready,
    Failed(runtime_portal::NotAvailable),
}

/// The shovel thread's body: owns a `current_thread` runtime + `LocalSet`
/// (capnp-rpc futures are `!Send`), stands up the SDK client, subscribes
/// its three streams, and forwards outbound requests. Exits when
/// `outbound_rx.recv()` returns `None` (manager dropped).
fn run_shovel(
    source: SdkConnectSource,
    identity: SdkJoinIdentity,
    outbound_rx: tokio_mpsc::Receiver<QueuedSdkCommand>,
    inbound_tx: InboundBridge,
    init_tx: mpsc::SyncSender<InitStatus>,
) {
    // Building `current_thread` is fallible (can OOM); surface as
    // Disabled — no service means the app runs local-only.
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = init_tx.send(InitStatus::Failed(
                runtime_portal::NotAvailable::ConnectionFailed(e),
            ));
            return;
        }
    };

    let local = LocalSet::new();
    local.block_on(&rt, async move {
        let client = match source.build_client() {
            Ok(c) => c,
            Err(e) => {
                let _ = init_tx.send(InitStatus::Failed(e));
                return;
            }
        };
        // The transport is ready. The manager queues its primary Hello next
        // and waits for `PrimaryReady` before returning it to the caller.
        let _ = init_tx.send(InitStatus::Ready);

        drive_shovel(client, identity, outbound_rx, inbound_tx).await;
    });
}

/// SDK-side event loop: dispatches outbound requests to SDK calls, and
/// spawns one subscription task per stream on the first `join_channel`.
// Not `Send`: `client` wraps a capnp-rpc handle (`Rc`-based, `!Send`), and
// this only ever runs inside `run_shovel`'s `LocalSet` on its own thread.
#[allow(
    clippy::future_not_send,
    clippy::too_many_lines,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn drive_shovel(
    client: runtime_collab::CollabClient,
    identity: SdkJoinIdentity,
    mut outbound_rx: tokio_mpsc::Receiver<QueuedSdkCommand>,
    inbound_tx: InboundBridge,
) {
    let SdkJoinIdentity {
        presentation,
        participant_kind,
        signer,
    } = identity;
    let mut routes = std::collections::HashMap::<String, SdkRoute>::new();
    let mut primary: Option<String> = None;
    let mut participant_handle: Option<String> = None;
    let mut primary_result = Err("primary collaboration route was not announced".to_string());

    while let Some(QueuedSdkCommand {
        command,
        _reservation,
    }) = outbound_rx.recv().await
    {
        if !inbound_tx.state.is_healthy() {
            break;
        }
        match command {
            SdkCommand::Primary(CollabRequest::Hello {
                app_id,
                cluster_id,
                record,
                handle,
            }) => {
                participant_handle = handle;
                let spec = RouteSpec {
                    app_id,
                    cluster_id,
                    record,
                };
                let key = spec.channel_id();
                primary_result = ensure_sdk_route(
                    &client,
                    &presentation,
                    participant_handle.as_deref(),
                    participant_kind,
                    &inbound_tx,
                    &mut routes,
                    &spec,
                )
                .await;
                primary = primary_result.is_ok().then_some(key);
            }
            SdkCommand::Primary(request) => {
                let Some(key) = primary.as_ref() else {
                    tracing::warn!("collab: request sent before the primary route joined");
                    continue;
                };
                if let Some(route) = routes.get_mut(key) {
                    dispatch_sdk_request(route, request, &signer, &inbound_tx).await;
                }
            }
            SdkCommand::Join(spec) => {
                let _ = ensure_sdk_route(
                    &client,
                    &presentation,
                    participant_handle.as_deref(),
                    participant_kind,
                    &inbound_tx,
                    &mut routes,
                    &spec,
                )
                .await;
            }
            SdkCommand::ToRoute(spec, request) => {
                let key = spec.channel_id();
                let joined = ensure_sdk_route(
                    &client,
                    &presentation,
                    participant_handle.as_deref(),
                    participant_kind,
                    &inbound_tx,
                    &mut routes,
                    &spec,
                )
                .await;
                if joined.is_ok()
                    && let Some(route) = routes.get_mut(&key)
                {
                    dispatch_sdk_request(route, request, &signer, &inbound_tx).await;
                }
            }
            SdkCommand::PrimaryReady(reply) => {
                let _ = reply.send(primary_result.clone());
            }
            SdkCommand::InstallEncryption { spec, group, reply } => {
                let result = install_sdk_encryption(
                    SdkJoinContext {
                        client: &client,
                        presentation: &presentation,
                        participant_handle: participant_handle.as_deref(),
                        participant_kind,
                        inbound_tx: &inbound_tx,
                    },
                    &mut routes,
                    spec,
                    *group,
                )
                .await;
                let _ = reply.send(result);
            }
            SdkCommand::PublishActionDirectory { app_id, requests } => {
                let Some(key) = primary.as_ref() else {
                    tracing::warn!("collab: directory published before the primary route joined");
                    continue;
                };
                let Some(route) = routes.get_mut(key) else {
                    tracing::warn!(
                        "collab: primary route disappeared before directory publication"
                    );
                    continue;
                };
                let directory = capnp_rpc::new_client(ActionDirectoryServer { requests });
                if let Err(error) = route
                    .channel
                    .publish_action_directory(&app_id, directory)
                    .await
                {
                    tracing::warn!(%error, "collab: publishing pull resource/action directory failed");
                }
            }
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc routes are !Send and this runs only on the shovel LocalSet"
)]
async fn install_sdk_encryption(
    context: SdkJoinContext<'_>,
    routes: &mut std::collections::HashMap<String, SdkRoute>,
    spec: RouteSpec,
    group: channel_crypto::ChannelGroup,
) -> Result<(), String> {
    let key = spec.channel_id();
    ensure_sdk_route(
        context.client,
        context.presentation,
        context.participant_handle,
        context.participant_kind,
        context.inbound_tx,
        routes,
        &spec,
    )
    .await?;
    let route = routes
        .get_mut(&key)
        .ok_or_else(|| "joined MLS route disappeared".to_string())?;
    *route
        .encryption
        .lock()
        .map_err(|_| "MLS endpoint state is unavailable after a poisoned lock".to_string())? =
        Some(group);
    Ok(())
}

struct SdkJoinContext<'a> {
    client: &'a runtime_collab::CollabClient,
    presentation: &'a [u8],
    participant_handle: Option<&'a str>,
    participant_kind: &'a str,
    inbound_tx: &'a InboundBridge,
}

struct SdkRoute {
    spec: RouteSpec,
    channel: runtime_collab::ChannelHandle,
    presence: Option<runtime_collab::PresenceHandle>,
    workspace_controls: std::collections::HashMap<u64, runtime_collab::WorkspaceControlHandle>,
    encryption: std::sync::Arc<std::sync::Mutex<Option<channel_crypto::ChannelGroup>>>,
}

// Not `Send`: `client`/`channel` wrap capnp-rpc handles, which are
// `Rc`-based and inherently `!Send` — this runs only on the shovel
// thread's `LocalSet` (see `run_shovel`), never crossing a thread
// boundary, so the non-`Send` future is by design, not an oversight.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn ensure_sdk_route(
    client: &runtime_collab::CollabClient,
    identity_bytes: &[u8],
    participant_handle: Option<&str>,
    participant_kind: &str,
    inbound_tx: &InboundBridge,
    routes: &mut std::collections::HashMap<String, SdkRoute>,
    spec: &RouteSpec,
) -> Result<(), String> {
    let channel_id = spec.channel_id();
    if routes.contains_key(&channel_id) {
        return Ok(());
    }
    let channel = match client
        .join_channel_as_kind(
            &channel_id,
            identity_bytes,
            participant_handle.unwrap_or(""),
            participant_kind,
        )
        .await
    {
        Ok(channel) => channel,
        Err(error) => {
            tracing::warn!(
                "collab: joining channel {channel_id} failed ({error}); route unavailable"
            );
            return Err(format!("joining collaboration route failed: {error}"));
        }
    };

    // The catalogue is level-triggered and its first snapshot is the route's
    // decision baseline. Enqueue it before subscribing to document traffic,
    // so a backlogged snapshot cannot make an agent decide against stale
    // local fallback data immediately after construction.
    spawn_action_catalogue_stream(&channel, inbound_tx).await?;
    let encryption = std::sync::Arc::new(std::sync::Mutex::new(None));
    spawn_op_stream(&channel, inbound_tx, std::sync::Arc::clone(&encryption)).await;
    spawn_context_stream(&channel, inbound_tx).await;
    spawn_action_stream(&channel, inbound_tx).await;
    spawn_invocation_stream(&channel, inbound_tx).await;
    // This intentionally fails quietly for person sessions. An admitted
    // agent gets a separate broker-addressed queue; a document agent never
    // has to infer task work from room traffic.
    spawn_task_run_stream(&channel, inbound_tx).await;
    let presence = spawn_presence_stream(&channel, inbound_tx).await;

    routes.insert(
        channel_id,
        SdkRoute {
            spec: spec.clone(),
            channel,
            presence,
            workspace_controls: std::collections::HashMap::new(),
            encryption,
        },
    );
    Ok(())
}

/// Bridge `channel`'s doc/incremental op stream into [`CollabEvent`]s on
/// `inbound_tx`, spawned as a `LocalSet` task so it doesn't block the
/// caller's route setup.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_op_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
    encryption: std::sync::Arc<std::sync::Mutex<Option<channel_crypto::ChannelGroup>>>,
) {
    if let Ok(stream) = channel.subscribe().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(event) = stream.read().await {
                let translated = match postcard::from_bytes::<SignedOp>(&event.op) {
                    Ok(envelope) => {
                        match signed_document_event(envelope, event.from_user_id, &encryption) {
                            Ok(event) => event,
                            Err(error) => {
                                tracing::warn!(
                                    %error,
                                    "collab: signed op has no typed document payload; dropping"
                                );
                                continue;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            "collab: signed op is not postcard SignedOp ({error}); dropping"
                        );
                        continue;
                    }
                };
                if !tx.send_durable(translated) {
                    break;
                }
            }
        });
    }
}

/// Bridge `channel`'s collaborator/permissions context stream into
/// [`CollabEvent::Context`] on `inbound_tx`.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_context_stream(channel: &runtime_collab::ChannelHandle, inbound_tx: &InboundBridge) {
    if let Ok(stream) = channel.subscribe_context().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(context) = stream.read().await {
                if let Some(peer_id) = context.follow_changed {
                    if !tx.send_durable(CollabEvent::FollowChanged { peer_id }) {
                        break;
                    }
                    continue;
                }
                let collaborators = context
                    .collaborators
                    .into_iter()
                    .map(|collaborator| Collaborator {
                        id: collaborator.user_id,
                        name: collaborator.display_name,
                        color: unpack_color(collaborator.color),
                        is_agent: collaborator.is_agent,
                        is_self: collaborator.is_self,
                    })
                    .collect();
                if !tx.send_durable(CollabEvent::Context {
                    collaborators,
                    permissions: context.permissions,
                }) {
                    break;
                }
            }
        });
    }
}

/// Bridge `channel`'s action stream into [`CollabEvent::ActionArrived`] on
/// `inbound_tx`.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_action_stream(channel: &runtime_collab::ChannelHandle, inbound_tx: &InboundBridge) {
    if let Ok(stream) = channel.subscribe_actions().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(action) = stream.read().await {
                if !tx.send_durable(CollabEvent::ActionArrived {
                    type_id: action.type_id,
                    payload: action.payload,
                    from_user_id: action.from_user_id,
                }) {
                    break;
                }
            }
        });
    }
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_invocation_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_invocations().await {
        let tx = inbound_tx.clone();
        let cancellation_channel = channel.clone();
        tokio::task::spawn_local(async move {
            while let Ok(invocation) = stream.read().await {
                let id = invocation.id;
                let context = parse_invocation_context(id, &invocation.context_json);
                if !tx.send_durable(CollabEvent::Invocation {
                    id,
                    resource: invocation.resource,
                    action: invocation.action,
                    argument: invocation.argument,
                    actor: invocation.actor,
                    context: context.map(Box::new),
                }) {
                    break;
                }
                let cancellation_tx = tx.clone();
                let channel = cancellation_channel.clone();
                tokio::task::spawn_local(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        match channel.invocation_cancellation(id).await {
                            Ok(false) => {}
                            Ok(true) => {
                                let _ = cancellation_tx
                                    .send_durable(CollabEvent::InvocationCancellation { id });
                                break;
                            }
                            // The broker removes terminal invocations, so an
                            // unknown-id error is also the watcher stop signal.
                            Err(_) => break,
                        }
                    }
                });
            }
        });
    }
}

fn parse_invocation_context(id: u64, json: &str) -> Option<qos_actions::ActionContext> {
    if json.trim().is_empty() {
        return None;
    }
    match serde_json::from_str(json) {
        Ok(context) => Some(context),
        Err(error) => {
            tracing::warn!(%error, id, "collab: ignoring invalid invocation context");
            None
        }
    }
}

#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_action_catalogue_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) -> Result<(), String> {
    let stream = channel
        .subscribe_action_catalogue()
        .await
        .map_err(|error| format!("subscribing to action catalogue failed: {error}"))?;
    let initial = stream
        .read()
        .await
        .map_err(|error| format!("reading initial action catalogue failed: {error}"))?;
    let catalogue = serde_json::from_str::<BrokerActionCatalogueWire>(&initial)
        .map_err(|error| format!("decoding initial action catalogue failed: {error}"))?;
    if !inbound_tx.send_durable(CollabEvent::ActionCatalogue {
        surfaces: catalogue.surfaces,
    }) {
        return Err("delivering initial action catalogue failed".to_string());
    }

    let tx = inbound_tx.clone();
    tokio::task::spawn_local(async move {
        while let Ok(snapshot) = stream.read().await {
            match serde_json::from_str::<BrokerActionCatalogueWire>(&snapshot) {
                Ok(catalogue) => {
                    if !tx.send_durable(CollabEvent::ActionCatalogue {
                        surfaces: catalogue.surfaces,
                    }) {
                        break;
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "collab: invalid broker action catalogue");
                }
            }
        }
    });
    Ok(())
}

/// Bridge broker-addressed task leases only when this connection is an
/// admitted agent. The server refuses the subscription for people and for
/// unadmitted agent identities, leaving their normal collaboration intact.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send and are driven only by the shovel LocalSet"
)]
async fn spawn_task_run_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) {
    if let Ok(stream) = channel.subscribe_task_runs().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(run) = stream.read().await {
                if !tx.send_durable(CollabEvent::TaskRun(run)) {
                    break;
                }
            }
        });
    }
}

/// Open `channel`'s presence handle and, if available, bridge its stream
/// into translated [`CollabEvent`]s on `inbound_tx`. Returns the handle so
/// the caller can store it on the route (needed for outbound presence
/// sends), regardless of whether the inbound stream subscribed.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn spawn_presence_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &InboundBridge,
) -> Option<runtime_collab::PresenceHandle> {
    match channel.presence().await {
        Ok(handle) => {
            if let Ok(stream) = handle.subscribe_all().await {
                let tx = inbound_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Ok(event) = stream.read().await {
                        if let Some(event) = translate_presence_event(event)
                            && !tx.send_presence(event)
                        {
                            break;
                        }
                    }
                });
            }
            Some(handle)
        }
        Err(_) => None,
    }
}

static LAST_CERTIFIED_DOCUMENT_SEQ: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn next_certified_document_seq(now: std::time::SystemTime) -> u64 {
    use std::sync::atomic::Ordering;

    let wall = now
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_micros()).unwrap_or(u64::MAX)
        });
    let mut observed = LAST_CERTIFIED_DOCUMENT_SEQ.load(Ordering::Relaxed);
    loop {
        let candidate = wall.max(observed.saturating_add(1));
        match LAST_CERTIFIED_DOCUMENT_SEQ.compare_exchange_weak(
            observed,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return candidate,
            Err(actual) => observed = actual,
        }
    }
}

/// Sign on the shovel thread through the workspace identity service, then use
/// the one signed Cap'n Proto verb. Missing or denied certification never
/// downgrades the document to unsigned traffic.
#[allow(
    clippy::future_not_send,
    reason = "the route contains a capnp-rpc handle and runs only on the shovel LocalSet"
)]
async fn send_document_mutation(
    route: &SdkRoute,
    signer: &SdkDocumentSigner,
    mutation: collab_types::DocumentMutation,
) {
    let workspace_id = route.spec.cluster_id.to_string();
    let record_id = route.spec.record.as_deref().unwrap_or("");
    let cleartext = match mutation.encode() {
        Ok(payload) => payload,
        Err(error) => {
            tracing::warn!(%error, "collab: typed document mutation would not encode; dropping");
            return;
        }
    };
    let Some((payload, encryption_metadata)) =
        prepare_document_payload(&route.encryption, cleartext)
    else {
        return;
    };
    let now = std::time::SystemTime::now();
    let author = match signer {
        #[cfg(unix)]
        SdkDocumentSigner::Certified(identity) => identity.signing_public_key(),
        SdkDocumentSigner::Harness(key) => key.verifying_key().to_bytes(),
    };
    let artifact = collab_types::document_artifact_id(&route.spec.app_id, &workspace_id, record_id);
    let author = collab_types::Ed25519PublicKey::from_bytes(author);
    let attestation =
        collab_types::document_attestation_id(&route.spec.app_id, &workspace_id, record_id);
    let seq = next_certified_document_seq(now);
    let mut op = match encryption_metadata {
        Some(metadata) => SignedOp::new_unsigned_with_encryption(
            payload,
            artifact,
            author,
            attestation,
            collab_types::OpClass::Splice,
            now,
            seq,
            metadata,
        ),
        None => SignedOp::new_unsigned(
            payload,
            artifact,
            author,
            attestation,
            collab_types::OpClass::Splice,
            now,
            seq,
        ),
    };
    let unsigned_op = match postcard::to_stdvec(&op) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%error, "collab: unsigned document envelope would not encode; dropping");
            return;
        }
    };
    let signature = match signer {
        #[cfg(unix)]
        SdkDocumentSigner::Certified(identity) => match identity.sign_collaboration_op(
            &workspace_id,
            &route.spec.app_id,
            record_id,
            &unsigned_op,
        ) {
            Ok(signature) => signature,
            Err(error) => {
                tracing::warn!(
                    ?error,
                    "collab: identity service denied document operation; dropping"
                );
                return;
            }
        },
        SdkDocumentSigner::Harness(key) => {
            use ed25519_dalek::Signer as _;
            let signing_bytes = match op.signing_bytes() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "collab: document op would not canonicalize; dropping");
                    return;
                }
            };
            key.sign(&signing_bytes).to_bytes()
        }
    };
    op.signature = collab_types::Signature::from_bytes(signature);
    match postcard::to_stdvec(&op) {
        Ok(bytes) => {
            let _ = route.channel.send(&bytes).await;
        }
        Err(error) => {
            tracing::warn!(%error, "collab: signed document envelope would not encode; dropping");
        }
    }
}

fn prepare_document_payload(
    encryption: &std::sync::Arc<std::sync::Mutex<Option<channel_crypto::ChannelGroup>>>,
    cleartext: Vec<u8>,
) -> Option<(Vec<u8>, Option<collab_types::EncryptionMetadata>)> {
    let Ok(mut guard) = encryption.lock() else {
        tracing::warn!("collab: MLS endpoint lock poisoned; dropping mutation");
        return None;
    };
    let Some(group) = guard.as_mut() else {
        return Some((cleartext, None));
    };
    let epoch = group.epoch();
    match group.encrypt(&cleartext) {
        Ok(ciphertext) => Some((
            ciphertext,
            Some(collab_types::EncryptionMetadata {
                epoch,
                channel: group.channel_id(),
            }),
        )),
        Err(error) => {
            tracing::warn!(%error, "collab: MLS encryption failed; dropping mutation");
            None
        }
    }
}

/// Shovel-thread Cap'n Proto endpoint for an application's pull directory.
/// It deliberately contains no `Widget`, `App`, or document reference.  The
/// only path to authority is a one-shot request answered by App's main-thread
/// pump below.
struct ActionDirectoryServer {
    requests: mpsc::SyncSender<ActionDirectoryRequest>,
}

fn directory_unavailable() -> runtime_collab::ActionDirectoryResolution {
    runtime_collab::ActionDirectoryResolution::Unavailable
}

fn write_directory_descriptor(
    mut out: runtime_collab::collab_capnp::exported_action::Builder<'_>,
    descriptor: &runtime_collab::ExportedAction,
) {
    out.set_resource(&descriptor.resource);
    out.set_action(&descriptor.action);
    out.set_resource_kind(&descriptor.resource_kind);
    out.set_resource_label(&descriptor.resource_label);
    out.set_action_label(&descriptor.action_label);
    out.set_description(descriptor.description.as_deref().unwrap_or(""));
    out.set_operation_kind(&descriptor.operation_kind);
    out.set_params_json(&descriptor.params_json);
    out.set_contract_json(&descriptor.contract_json);
    let mut input = out.init_input();
    match &descriptor.input {
        runtime_collab::ExportedInput::None => input.set_none(()),
        runtime_collab::ExportedInput::Text {
            prompt,
            placeholder,
            wants_content,
        } => {
            let mut text = input.init_text();
            text.set_prompt(prompt);
            text.set_placeholder(placeholder);
            text.set_wants_content(*wants_content);
        }
    }
}

fn write_directory_row(
    mut out: runtime_collab::collab_capnp::action_directory_row::Builder<'_>,
    row: &runtime_collab::ActionDirectoryRow,
) {
    out.set_parent_resource(&row.parent_resource);
    out.set_resource(&row.resource);
    out.set_resource_kind(&row.resource_kind);
    out.set_resource_label(&row.resource_label);
    out.set_action(row.action.as_deref().unwrap_or(""));
    out.set_has_children(row.has_children);
    out.set_visibility_action(row.visibility_action.as_deref().unwrap_or(""));
    out.set_visibility_contract_json(&row.visibility_contract_json);
    if let Some(descriptor) = &row.descriptor {
        write_directory_descriptor(out.reborrow().init_descriptor(), descriptor);
    }
}

#[allow(
    refining_impl_trait,
    reason = "capnp generated server traits accept the concrete Promise type used by every existing RPC endpoint"
)]
impl runtime_collab::collab_capnp::action_directory::Server for ActionDirectoryServer {
    fn query(
        self: std::rc::Rc<Self>,
        params: runtime_collab::collab_capnp::action_directory::QueryParams,
        mut results: runtime_collab::collab_capnp::action_directory::QueryResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let query = match params.get().and_then(
            runtime_collab::collab_capnp::action_directory::query_params::Reader::get_query,
        ) {
            Ok(query) => {
                let text = |value: capnp::Result<capnp::text::Reader<'_>>| {
                    value
                        .and_then(|value| {
                            value
                                .to_str()
                                .map_err(|error| capnp::Error::failed(error.to_string()))
                        })
                        .map(str::to_owned)
                };
                match (
                    text(query.get_revision()),
                    query.get_cursor().map(<[u8]>::to_vec),
                    text(query.get_needle()),
                    text(query.get_parent_resource()),
                ) {
                    (Ok(revision), Ok(cursor), Ok(needle), Ok(parent_resource)) => {
                        Ok(runtime_collab::ActionDirectoryQuery {
                            peer_id: query.get_peer_id(),
                            revision,
                            cursor,
                            needle,
                            parent_resource,
                            max_results: query.get_max_results(),
                            max_bytes: query.get_max_bytes(),
                            max_work: query.get_max_work(),
                            max_depth: query.get_max_depth(),
                        })
                    }
                    (Err(error), ..)
                    | (_, Err(error), ..)
                    | (_, _, Err(error), ..)
                    | (_, _, _, Err(error)) => Err(error),
                }
            }
            Err(error) => Err(error),
        };
        let query = match query {
            Ok(query) => query,
            Err(error) => return capnp::capability::Promise::err(error),
        };
        let requests = self.requests.clone();
        capnp::capability::Promise::from_future(async move {
            let (reply, response) = tokio::sync::oneshot::channel();
            requests
                .try_send(ActionDirectoryRequest::Query { query, reply })
                .map_err(|_| {
                    capnp::Error::failed(
                        "application action directory is busy or unavailable".into(),
                    )
                })?;
            let page = tokio::time::timeout(ACTION_DIRECTORY_REPLY_TIMEOUT, response)
                .await
                .map_err(|_| capnp::Error::failed("application action directory timed out".into()))?
                .map_err(|_| {
                    capnp::Error::failed("application action directory is unavailable".into())
                })?;
            let mut out = results.get().init_page();
            out.set_revision(&page.revision);
            out.set_next_cursor(&page.next_cursor);
            out.set_exhausted(page.exhausted);
            out.set_stale_cursor(page.stale_cursor);
            out.set_budget_exhausted(page.budget_exhausted);
            let len = u32::try_from(page.rows.len())
                .map_err(|_| capnp::Error::failed("too many directory rows".into()))?;
            let mut rows = out.reborrow().init_rows(len);
            for (index, row) in page.rows.iter().enumerate() {
                write_directory_row(
                    rows.reborrow()
                        .get(u32::try_from(index).unwrap_or(u32::MAX)),
                    row,
                );
            }
            Ok(())
        })
    }

    fn resolve(
        self: std::rc::Rc<Self>,
        params: runtime_collab::collab_capnp::action_directory::ResolveParams,
        mut results: runtime_collab::collab_capnp::action_directory::ResolveResults,
    ) -> capnp::capability::Promise<(), capnp::Error> {
        let request = params.get().and_then(|request| {
            let text = |value: capnp::Result<capnp::text::Reader<'_>>| {
                value
                    .and_then(|value| {
                        value
                            .to_str()
                            .map_err(|error| capnp::Error::failed(error.to_string()))
                    })
                    .map(str::to_owned)
            };
            Ok((
                text(request.get_revision())?,
                text(request.get_resource())?,
                text(request.get_action())?,
            ))
        });
        let (revision, resource, action) = match request {
            Ok(request) => request,
            Err(error) => return capnp::capability::Promise::err(error),
        };
        let requests = self.requests.clone();
        capnp::capability::Promise::from_future(async move {
            let (reply, response) = tokio::sync::oneshot::channel();
            let reply_value = if requests
                .try_send(ActionDirectoryRequest::Resolve {
                    revision,
                    resource,
                    action,
                    reply,
                })
                .is_err()
            {
                directory_unavailable()
            } else {
                tokio::time::timeout(ACTION_DIRECTORY_REPLY_TIMEOUT, response)
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_else(directory_unavailable)
            };
            let mut out = results.get().init_resolution();
            match reply_value {
                runtime_collab::ActionDirectoryResolution::Found(row) => {
                    write_directory_row(out.reborrow().init_found(), &row);
                }
                runtime_collab::ActionDirectoryResolution::NotFound => out.set_not_found(()),
                runtime_collab::ActionDirectoryResolution::StaleRevision => {
                    out.set_stale_revision(());
                }
                runtime_collab::ActionDirectoryResolution::Unavailable => out.set_unavailable(()),
            }
            Ok(())
        })
    }
}

// Not `Send`: `route.channel` wraps a capnp-rpc handle (`Rc`-based,
// `!Send`), and this only ever runs inside the shovel thread's `LocalSet`.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
#[allow(
    clippy::too_many_lines,
    reason = "the request variants share one ordered dispatch boundary onto the SDK route"
)]
async fn dispatch_sdk_request(
    route: &mut SdkRoute,
    request: CollabRequest,
    signer: &SdkDocumentSigner,
    inbound_tx: &InboundBridge,
) {
    match request {
        CollabRequest::Hello { .. } => {}
        CollabRequest::SignedOperation { envelope } => {
            if let Ok(bytes) = postcard::to_stdvec(&*envelope) {
                let _ = route.channel.send(&bytes).await;
            }
        }
        CollabRequest::DocumentSnapshot { data } => {
            send_document_mutation(
                route,
                signer,
                collab_types::DocumentMutation::Snapshot(data),
            )
            .await;
        }
        CollabRequest::DocumentIncremental { data } => {
            send_document_mutation(
                route,
                signer,
                collab_types::DocumentMutation::Incremental(data),
            )
            .await;
        }
        CollabRequest::Presence { key, payload } => {
            let Some(presence) = route.presence.as_ref() else {
                return;
            };
            let key_bytes = postcard::to_stdvec(&key).unwrap_or_default();
            match payload {
                PresencePayload::Pointer { x, y } => {
                    let _ = presence.publish_pointer(&key_bytes, x, y).await;
                }
                PresencePayload::Caret { pos } => {
                    let _ = presence.publish_caret(&key_bytes, pos as u64).await;
                }
                PresencePayload::Selection { start, end } => {
                    let _ = presence
                        .publish_selection(&key_bytes, start as u64, end as u64)
                        .await;
                }
                PresencePayload::Typing { start, end } => {
                    let _ = presence
                        .publish_typing(&key_bytes, start as u64, end as u64)
                        .await;
                }
                PresencePayload::Arrived => {
                    let _ = presence.publish_arrived(&key_bytes).await;
                }
                PresencePayload::Left => {
                    let _ = presence.publish_left(&key_bytes).await;
                }
                // Shell chrome originates this authenticated visual cue
                // directly on the peer-presence route. Apps cannot turn it
                // into a viewport command through this generic SDK bridge.
                PresencePayload::Following { .. } => {}
                PresencePayload::Interaction(update) => {
                    if let Ok(json) = serde_json::to_string(&update) {
                        let _ = presence.publish_interaction(&key_bytes, &json).await;
                    }
                }
            }
        }
        CollabRequest::FetchScrollback {
            artifact,
            before_seq,
            max,
        } => {
            let (ops, decrypted_payloads, exhausted) = match route
                .channel
                .fetch_scrollback_for(artifact.0.as_bytes(), before_seq, u32::from(max))
                .await
            {
                Ok(batch) => {
                    let mut payloads = Vec::new();
                    let ops = batch
                        .ops
                        .into_iter()
                        .filter_map(|bytes| postcard::from_bytes::<SignedOp>(&bytes).ok())
                        .filter_map(|op| match decrypt_document_payload(&op, &route.encryption) {
                            Ok(payload) => {
                                payloads.push(payload);
                                Some(Box::new(op))
                            }
                            Err(error) => {
                                tracing::warn!(%error, "collab: dropping undecryptable scrollback envelope");
                                None
                            }
                        })
                        .collect();
                    (ops, payloads, batch.done)
                }
                Err(_) => (Vec::new(), Vec::new(), true),
            };
            let _ = inbound_tx.send_durable(CollabEvent::ScrollbackBatch {
                artifact,
                ops,
                decrypted_payloads,
                exhausted,
            });
        }
        CollabRequest::PublishActions { app_id, actions } => {
            let actions = actions
                .into_iter()
                .map(|action| runtime_collab::ExportedAction {
                    resource: action.resource,
                    action: action.action,
                    resource_kind: action.resource_kind,
                    resource_label: action.resource_label,
                    action_label: action.action_label,
                    description: action.description,
                    operation_kind: action.operation_kind,
                    params_json: action.params_json,
                    contract_json: action.contract_json,
                    input: match action.input {
                        ExportedInput::None => runtime_collab::ExportedInput::None,
                        ExportedInput::Text {
                            prompt,
                            placeholder,
                            wants_content,
                        } => runtime_collab::ExportedInput::Text {
                            prompt,
                            placeholder,
                            wants_content,
                        },
                    },
                })
                .collect::<Vec<_>>();
            if let Err(error) = route.channel.publish_actions(&app_id, &actions).await {
                tracing::warn!(%error, "collab: publishing resource/action surface failed");
            }
        }
        CollabRequest::ReportContinuation {
            app_id,
            schema_version,
            metadata,
        } => {
            if let Err(error) = route
                .channel
                .report_continuation(&app_id, schema_version, &metadata)
                .await
            {
                tracing::warn!(%error, "collab: reporting continuation metadata failed");
            }
        }
        CollabRequest::ClearContinuation { app_id } => {
            if let Err(error) = route.channel.clear_continuation(&app_id).await {
                tracing::warn!(%error, "collab: clearing continuation metadata failed");
            }
        }
        CollabRequest::OpenWorkspaceControl {
            session_id,
            target_device,
        } => match route.channel.open_workspace_control(target_device).await {
            Ok(handle) => {
                let reader = handle.clone();
                let tx = inbound_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Ok(event) = reader.read().await {
                        let terminal =
                            matches!(event, runtime_collab::WorkspaceControlEvent::Closed(_));
                        if !tx.send_durable(CollabEvent::WorkspaceControl { session_id, event })
                            || terminal
                        {
                            break;
                        }
                    }
                });
                route.workspace_controls.insert(session_id, handle);
            }
            Err(error) => {
                let _ = inbound_tx.send_durable(CollabEvent::WorkspaceControl {
                    session_id,
                    event: runtime_collab::WorkspaceControlEvent::Closed(
                        runtime_collab::WorkspaceControlCloseReason::Denied(error.to_string()),
                    ),
                });
            }
        },
        CollabRequest::WorkspaceControlInput { session_id, event } => {
            if let Some(handle) = route.workspace_controls.get(&session_id)
                && let Err(error) = handle.send_input(&event).await
            {
                let _ = inbound_tx.send_durable(CollabEvent::WorkspaceControl {
                    session_id,
                    event: runtime_collab::WorkspaceControlEvent::Closed(
                        runtime_collab::WorkspaceControlCloseReason::Denied(error.to_string()),
                    ),
                });
            }
        }
        CollabRequest::CloseWorkspaceControl { session_id } => {
            if let Some(handle) = route.workspace_controls.remove(&session_id) {
                let _ = handle.close().await;
            }
        }
        CollabRequest::CompleteInvocation { id, code, message } => {
            if let Err(error) = route.channel.complete_invocation(id, &code, &message).await {
                tracing::warn!(%error, id, "collab: completing resource/action invocation failed");
            }
        }
        CollabRequest::UpdateInvocation { id, update } => match serde_json::to_string(&update) {
            Ok(json) => {
                if let Err(error) = route.channel.update_invocation(id, &json).await {
                    tracing::warn!(%error, id, "collab: updating action invocation failed");
                }
            }
            Err(error) => tracing::warn!(%error, id, "collab: encoding action update failed"),
        },
        CollabRequest::RecordLocalEffect { address, update } => {
            match serde_json::to_string(&update) {
                Ok(json) => {
                    if let Err(error) = route
                        .channel
                        .record_local_effect(&address.resource, &address.action, &json)
                        .await
                    {
                        tracing::warn!(%error, "collab: recording local effect receipt failed");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "collab: encoding local effect receipt failed");
                }
            }
        }
        CollabRequest::InvokePublishedAction {
            request_id,
            peer_id,
            resource,
            action,
            arguments,
            confirmation,
        } => {
            let argument = format!(
                "{}{}",
                qos_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(arguments)
            );
            let confirmation = confirmation.to_string();
            let result = route
                .channel
                .invoke_published_action(
                    peer_id,
                    &resource,
                    &action,
                    Some(&argument),
                    &confirmation,
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|(id, update)| {
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_durable(CollabEvent::PublishedInvocationResult { request_id, result });
        }
        CollabRequest::PublishedInvocationStatus { request_id, id } => {
            let result = route
                .channel
                .published_invocation_status(id)
                .await
                .map_err(|error| error.to_string())
                .and_then(|update| {
                    let update = update.ok_or_else(|| "unknown invocation id".to_string())?;
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_durable(CollabEvent::PublishedInvocationResult { request_id, result });
        }
        CollabRequest::CancelPublishedInvocation { request_id, id } => {
            let result = route
                .channel
                .cancel_published_invocation(id)
                .await
                .map_err(|error| error.to_string())
                .and_then(|update| {
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_durable(CollabEvent::PublishedInvocationResult { request_id, result });
        }
        CollabRequest::LaunchPublishedApplication { request_id, app_id } => {
            let result = route
                .channel
                .launch_published_application(&app_id)
                .await
                .map_err(|error| error.to_string())
                .and_then(|(id, update)| {
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_durable(CollabEvent::PublishedInvocationResult { request_id, result });
        }
        CollabRequest::ReportTaskRun {
            run_id,
            lease,
            update,
        } => {
            if let Err(error) = route.channel.report_task_run(run_id, &lease, &update).await {
                tracing::warn!(%error, run_id, "collab: reporting task run update failed");
            }
        }
        CollabRequest::InvokeTaskAction {
            request_id,
            run_id,
            lease,
            peer_id,
            resource,
            action,
            arguments,
        } => {
            let argument = format!(
                "{}{}",
                qos_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::Value::Object(arguments)
            );
            let result = route
                .channel
                .invoke_task_action(
                    run_id,
                    &lease,
                    peer_id,
                    &resource,
                    &action,
                    Some(&argument),
                    "",
                )
                .await
                .map_err(|error| error.to_string())
                .and_then(|(id, update)| {
                    serde_json::from_str(&update)
                        .map(|update| (id, update))
                        .map_err(|error| error.to_string())
                });
            let _ = inbound_tx
                .send_durable(CollabEvent::TaskActionInvocationResult { request_id, result });
        }
        CollabRequest::TaskRunCancellation {
            request_id,
            run_id,
            lease,
        } => {
            let result = route
                .channel
                .task_run_cancelled(run_id, &lease)
                .await
                .map_err(|error| error.to_string());
            let _ =
                inbound_tx.send_durable(CollabEvent::TaskRunCancellation { request_id, result });
        }
    }
}
fn signed_document_event(
    envelope: SignedOp,
    from_user_id: String,
    encryption: &std::sync::Arc<std::sync::Mutex<Option<channel_crypto::ChannelGroup>>>,
) -> Result<CollabEvent, collab_types::DocumentMutationError> {
    let decrypted_payload = decrypt_document_payload(&envelope, encryption)?;
    Ok(CollabEvent::Document {
        envelope: Box::new(envelope),
        from_user_id,
        decrypted_payload,
    })
}

fn decrypt_document_payload(
    envelope: &SignedOp,
    encryption: &std::sync::Arc<std::sync::Mutex<Option<channel_crypto::ChannelGroup>>>,
) -> Result<Option<Vec<u8>>, collab_types::DocumentMutationError> {
    if let Some(metadata) = envelope.encryption {
        let mut guard = encryption
            .lock()
            .map_err(|_| collab_types::DocumentMutationError::RouteMismatch)?;
        let group = guard
            .as_mut()
            .ok_or(collab_types::DocumentMutationError::RouteMismatch)?;
        if group.channel_id() != metadata.channel || group.epoch() < metadata.epoch {
            return Err(collab_types::DocumentMutationError::RouteMismatch);
        }
        let plaintext = group
            .decrypt(&envelope.payload)
            .map_err(|_| collab_types::DocumentMutationError::RouteMismatch)?;
        drop(guard);
        let _mutation = collab_types::DocumentMutation::decode(&plaintext)?;
        Ok(Some(plaintext))
    } else {
        if encryption
            .lock()
            .map_err(|_| collab_types::DocumentMutationError::RouteMismatch)?
            .is_some()
        {
            // Once provisioned, accepting a plaintext envelope would be an
            // active downgrade. There is no opportunistic fallback.
            return Err(collab_types::DocumentMutationError::RouteMismatch);
        }
        let _mutation = collab_types::DocumentMutation::decode(&envelope.payload)?;
        Ok(None)
    }
}

/// Unpack the SDK's 0x00RRGGBB u32 into [R, G, B] floats in 0..1 — the
/// shape the legacy dialect already used, so existing consumers render
/// identically.
#[allow(
    clippy::cast_precision_loss,
    reason = "each channel is masked to 0..=255 by `& 0xFF`, always exactly representable in f32"
)]
fn unpack_color(packed: u32) -> [f32; 3] {
    let r = ((packed >> 16) & 0xFF) as f32 / 255.0;
    let g = ((packed >> 8) & 0xFF) as f32 / 255.0;
    let b = (packed & 0xFF) as f32 / 255.0;
    [r, g, b]
}

/// Translate an SDK [`runtime_collab::PresenceEvent`] into a
/// `CollabEvent`. Extracted so the unit test can exercise it without a broker.
fn translate_presence_event(evt: runtime_collab::PresenceEvent) -> Option<CollabEvent> {
    // Cursor keeps the legacy `user_id + x + y` shape; every other
    // variant must carry a decodable semantic `PresenceKey`.
    match evt.payload {
        runtime_collab::PresencePayload::Cursor { x, y } => Some(CollabEvent::RemoteCursorMoved {
            user_id: evt.user_id,
            x,
            y,
        }),
        other => {
            // An undecodable scope cannot truthfully identify a target. Drop
            // it instead of inventing a tab and making malformed presence
            // look like a collaborator left some unrelated surface.
            let key: PresenceKey = postcard::from_bytes(&evt.key_bytes).ok()?;
            let payload = match other {
                runtime_collab::PresencePayload::Caret { pos } => PresencePayload::Caret {
                    // A wire-supplied offset that doesn't fit `usize` on this
                    // target can't address anything real; clamp rather than
                    // wrap it into an unrelated in-range position.
                    pos: usize::try_from(pos).unwrap_or(usize::MAX),
                },
                runtime_collab::PresencePayload::Selection { start, end } => {
                    PresencePayload::Selection {
                        start: usize::try_from(start).unwrap_or(usize::MAX),
                        end: usize::try_from(end).unwrap_or(usize::MAX),
                    }
                }
                runtime_collab::PresencePayload::Typing { start, end } => PresencePayload::Typing {
                    start: usize::try_from(start).unwrap_or(usize::MAX),
                    end: usize::try_from(end).unwrap_or(usize::MAX),
                },
                runtime_collab::PresencePayload::Following { target } => {
                    PresencePayload::Following { target }
                }
                runtime_collab::PresencePayload::InteractionJson(json) => {
                    let update: qos_actions::SemanticInteraction =
                        serde_json::from_str(&json).ok()?;
                    update.validate().ok()?;
                    PresencePayload::Interaction(Box::new(update))
                }
                runtime_collab::PresencePayload::Arrived => PresencePayload::Arrived,
                // `Cursor` is fully handled and returned by the outer match, so
                // it never reaches here; folding it in with `Left` keeps this
                // exhaustive without a panic on a branch that cannot run.
                runtime_collab::PresencePayload::Left
                | runtime_collab::PresencePayload::Cursor { .. } => PresencePayload::Left,
                // Keeps the coordinate space its key carries, where
                // `Cursor` has nowhere to put one.
                runtime_collab::PresencePayload::Pointer { x, y } => {
                    PresencePayload::Pointer { x, y }
                }
            };
            Some(CollabEvent::RemotePresenceUpdate {
                user_id: evt.user_id,
                key,
                payload,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    // The `Fake*` capnp-rpc mock servers below implement generated `Server`
    // traits by returning the concrete `Promise<T, capnp::Error>` capnp-rpc
    // expects, which is more specific than (though it does satisfy) the
    // trait's generic `impl Future` return -- the exact "refining" this
    // lint polices, and the rustc note on it says to allow when intended.
    // Rewriting every method's return type to the verbose `impl
    // Future<Output = ...>` spelling wouldn't change any behaviour, only
    // make capnp-generated-shaped code look less like the rest of the
    // capnp-rpc code it's mocking.
    #![allow(refining_impl_trait)]

    use super::*;

    use std::cell::RefCell;
    use std::os::fd::IntoRawFd;
    use std::rc::Rc;
    use std::time::{Duration, Instant};

    use capnp::capability::Promise;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio::net::UnixStream as TokioUnixStream;
    use tokio::task::LocalSet as TokioLocalSet;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    use runtime_collab::PresencePayload as SdkPayload;
    use runtime_collab::raw::{
        action_catalogue_stream, channel_context, channel_handle, collab_service, context_stream,
        presence_event, presence_handle, presence_payload, presence_stream, scrollback_batch,
        stream,
    };

    #[derive(Default)]
    struct Captures {
        joined: Vec<(String, Vec<u8>)>,
        signed_ops: Vec<Vec<u8>>,
        cursor_publishes: Vec<(f32, f32)>,
        pointer_publishes: Vec<(Vec<u8>, f32, f32)>,
        caret_publishes: Vec<(Vec<u8>, u64)>,
        selection_publishes: Vec<(Vec<u8>, u64, u64)>,
        typing_publishes: Vec<(Vec<u8>, u64, u64)>,
        arrived: Vec<Vec<u8>>,
        left: Vec<Vec<u8>>,
        scrollback_requests: Vec<(Vec<u8>, u64, u32)>,
    }

    /// Shared between the test-thread's tokio `LocalSet` and the caller.
    /// `Arc<Mutex<_>>` (not `Rc<RefCell<_>>`) because the broker
    /// struct crosses a `thread::spawn` boundary before it's driven
    /// through the RPC system.
    type SharedCaptures = std::sync::Arc<std::sync::Mutex<Captures>>;

    #[derive(Clone)]
    struct SeedPresence {
        user_id: String,
        key_bytes: Vec<u8>,
        payload: SeedPayload,
    }

    #[derive(Clone)]
    enum SeedPayload {
        Cursor { x: f32, y: f32 },
        Pointer { x: f32, y: f32 },
        Caret { pos: u64 },
        Selection { start: u64, end: u64 },
        Typing { start: u64, end: u64 },
        Arrived,
        Left,
    }

    #[derive(Clone)]
    struct SeedContext {
        collaborators: Vec<(String, String, u32)>,
        permissions: Vec<String>,
    }

    #[derive(Clone)]
    struct SeedScrollback {
        ops: Vec<Vec<u8>>,
        earliest_seq: u64,
        latest_seq: u64,
        done: bool,
    }

    struct FakePresenceStream {
        backlog: Rc<RefCell<Vec<SeedPresence>>>,
    }

    impl presence_stream::Server for FakePresenceStream {
        fn read(
            self: Rc<Self>,
            _params: presence_stream::ReadParams,
            mut results: presence_stream::ReadResults,
        ) -> Promise<(), capnp::Error> {
            let mut backlog = self.backlog.borrow_mut();
            if backlog.is_empty() {
                // Never resolve — clients that outrun the backlog
                // simply wait forever, which mirrors the real broker.
                return Promise::from_future(std::future::pending::<Result<(), capnp::Error>>());
            }
            let seed = backlog.remove(0);
            let mut ev: presence_event::Builder = results.get().init_event();
            ev.set_user_id(&seed.user_id);
            ev.set_key_bytes(&seed.key_bytes);
            let mut payload: presence_payload::Builder = ev.init_payload();
            match seed.payload {
                SeedPayload::Cursor { x, y } => {
                    let mut b = payload.init_cursor();
                    b.set_x(x);
                    b.set_y(y);
                }
                SeedPayload::Pointer { x, y } => {
                    let mut b = payload.init_pointer();
                    b.set_x(x);
                    b.set_y(y);
                }
                SeedPayload::Caret { pos } => {
                    payload.init_caret().set_pos(pos);
                }
                SeedPayload::Selection { start, end } => {
                    let mut b = payload.init_selection();
                    b.set_start(start);
                    b.set_end(end);
                }
                SeedPayload::Typing { start, end } => {
                    let mut b = payload.init_typing();
                    b.set_start(start);
                    b.set_end(end);
                }
                SeedPayload::Arrived => payload.set_arrived(()),
                SeedPayload::Left => payload.set_left(()),
            }
            Promise::ok(())
        }
    }

    struct FakePresence {
        captures: SharedCaptures,
        presence_backlog: Rc<RefCell<Vec<SeedPresence>>>,
    }

    impl presence_handle::Server for FakePresence {
        fn publish_cursor(
            self: Rc<Self>,
            params: presence_handle::PublishCursorParams,
            _results: presence_handle::PublishCursorResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            self.captures
                .lock()
                .unwrap()
                .cursor_publishes
                .push((r.get_x(), r.get_y()));
            Promise::ok(())
        }

        fn publish_pointer(
            self: Rc<Self>,
            params: presence_handle::PublishPointerParams,
            _results: presence_handle::PublishPointerResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().pointer_publishes.push((
                k.to_vec(),
                r.get_x(),
                r.get_y(),
            ));
            Promise::ok(())
        }

        fn publish_caret(
            self: Rc<Self>,
            params: presence_handle::PublishCaretParams,
            _results: presence_handle::PublishCaretResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures
                .lock()
                .unwrap()
                .caret_publishes
                .push((k.to_vec(), r.get_pos()));
            Promise::ok(())
        }

        fn publish_selection(
            self: Rc<Self>,
            params: presence_handle::PublishSelectionParams,
            _results: presence_handle::PublishSelectionResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().selection_publishes.push((
                k.to_vec(),
                r.get_start(),
                r.get_end(),
            ));
            Promise::ok(())
        }

        fn publish_typing(
            self: Rc<Self>,
            params: presence_handle::PublishTypingParams,
            _results: presence_handle::PublishTypingResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().typing_publishes.push((
                k.to_vec(),
                r.get_start(),
                r.get_end(),
            ));
            Promise::ok(())
        }

        fn publish_arrived(
            self: Rc<Self>,
            params: presence_handle::PublishArrivedParams,
            _results: presence_handle::PublishArrivedResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().arrived.push(k.to_vec());
            Promise::ok(())
        }

        fn publish_left(
            self: Rc<Self>,
            params: presence_handle::PublishLeftParams,
            _results: presence_handle::PublishLeftResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else {
                return Promise::ok(());
            };
            let Ok(k) = r.get_key_bytes() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().left.push(k.to_vec());
            Promise::ok(())
        }

        fn subscribe_all(
            self: Rc<Self>,
            _params: presence_handle::SubscribeAllParams,
            mut results: presence_handle::SubscribeAllResults,
        ) -> Promise<(), capnp::Error> {
            let client: presence_stream::Client = capnp_rpc::new_client(FakePresenceStream {
                backlog: self.presence_backlog.clone(),
            });
            results.get().set_stream(client);
            Promise::ok(())
        }
    }

    /// `(op bytes, author)` pairs captured by a fake op/stream mock.
    type OpBacklog = Rc<RefCell<Vec<(Vec<u8>, String)>>>;

    struct FakeOpStream {
        backlog: OpBacklog,
    }

    impl stream::Server for FakeOpStream {
        fn read(
            self: Rc<Self>,
            _params: stream::ReadParams,
            mut results: stream::ReadResults,
        ) -> Promise<(), capnp::Error> {
            let mut backlog = self.backlog.borrow_mut();
            if backlog.is_empty() {
                return Promise::from_future(std::future::pending::<Result<(), capnp::Error>>());
            }
            let (op, from) = backlog.remove(0);
            let mut r = results.get();
            r.set_op(&op);
            r.set_from_user_id(&from);
            Promise::ok(())
        }
    }

    struct FakeContextStream {
        backlog: Rc<RefCell<Vec<SeedContext>>>,
    }

    impl context_stream::Server for FakeContextStream {
        fn read(
            self: Rc<Self>,
            _params: context_stream::ReadParams,
            mut results: context_stream::ReadResults,
        ) -> Promise<(), capnp::Error> {
            let mut backlog = self.backlog.borrow_mut();
            if backlog.is_empty() {
                return Promise::from_future(std::future::pending::<Result<(), capnp::Error>>());
            }
            let seed = backlog.remove(0);
            let mut ctx: channel_context::Builder = results.get().init_ctx();
            {
                let mut collabs = ctx.reborrow().init_collaborators(
                    u32::try_from(seed.collaborators.len()).unwrap_or(u32::MAX),
                );
                for (i, (uid, name, color)) in seed.collaborators.iter().enumerate() {
                    let mut c = collabs.reborrow().get(u32::try_from(i).unwrap_or(u32::MAX));
                    c.set_user_id(uid);
                    c.set_display_name(name);
                    c.set_color(*color);
                }
            }
            {
                let mut perms =
                    ctx.init_permissions(u32::try_from(seed.permissions.len()).unwrap_or(u32::MAX));
                for (i, p) in seed.permissions.iter().enumerate() {
                    perms.set(u32::try_from(i).unwrap_or(u32::MAX), p.as_str());
                }
            }
            Promise::ok(())
        }
    }

    struct FakeActionCatalogueStream {
        initial: RefCell<Option<String>>,
    }

    impl action_catalogue_stream::Server for FakeActionCatalogueStream {
        fn read(
            self: Rc<Self>,
            _params: action_catalogue_stream::ReadParams,
            mut results: action_catalogue_stream::ReadResults,
        ) -> Promise<(), capnp::Error> {
            let Some(initial) = self.initial.borrow_mut().take() else {
                return Promise::from_future(std::future::pending::<Result<(), capnp::Error>>());
            };
            results.get().set_catalogue_json(&initial);
            Promise::ok(())
        }
    }

    struct FakeChannel {
        captures: SharedCaptures,
        op_backlog: OpBacklog,
        presence_backlog: Rc<RefCell<Vec<SeedPresence>>>,
        context_backlog: Rc<RefCell<Vec<SeedContext>>>,
        scrollback_response: Rc<RefCell<Option<SeedScrollback>>>,
    }

    impl channel_handle::Server for FakeChannel {
        fn send(
            self: Rc<Self>,
            params: channel_handle::SendParams,
            _results: channel_handle::SendResults,
        ) -> Promise<(), capnp::Error> {
            let op = match params
                .get()
                .and_then(channel_handle::send_params::Reader::get_op)
            {
                Ok(o) => o.to_vec(),
                Err(e) => return Promise::err(e),
            };
            self.captures.lock().unwrap().signed_ops.push(op);
            Promise::ok(())
        }

        fn subscribe(
            self: Rc<Self>,
            _params: channel_handle::SubscribeParams,
            mut results: channel_handle::SubscribeResults,
        ) -> Promise<(), capnp::Error> {
            let client: stream::Client = capnp_rpc::new_client(FakeOpStream {
                backlog: self.op_backlog.clone(),
            });
            results.get().set_stream(client);
            Promise::ok(())
        }

        fn presence(
            self: Rc<Self>,
            _params: channel_handle::PresenceParams,
            mut results: channel_handle::PresenceResults,
        ) -> Promise<(), capnp::Error> {
            let client: presence_handle::Client = capnp_rpc::new_client(FakePresence {
                captures: self.captures.clone(),
                presence_backlog: self.presence_backlog.clone(),
            });
            results.get().set_handle(client);
            Promise::ok(())
        }

        fn retired_unsigned_snapshot(
            self: Rc<Self>,
            _params: channel_handle::RetiredUnsignedSnapshotParams,
            _results: channel_handle::RetiredUnsignedSnapshotResults,
        ) -> Promise<(), capnp::Error> {
            Promise::ok(())
        }

        fn retired_unsigned_incremental(
            self: Rc<Self>,
            _params: channel_handle::RetiredUnsignedIncrementalParams,
            _results: channel_handle::RetiredUnsignedIncrementalResults,
        ) -> Promise<(), capnp::Error> {
            Promise::ok(())
        }

        fn subscribe_context(
            self: Rc<Self>,
            _params: channel_handle::SubscribeContextParams,
            mut results: channel_handle::SubscribeContextResults,
        ) -> Promise<(), capnp::Error> {
            let client: context_stream::Client = capnp_rpc::new_client(FakeContextStream {
                backlog: self.context_backlog.clone(),
            });
            results.get().set_stream(client);
            Promise::ok(())
        }

        fn subscribe_action_catalogue(
            self: Rc<Self>,
            _params: channel_handle::SubscribeActionCatalogueParams,
            mut results: channel_handle::SubscribeActionCatalogueResults,
        ) -> Promise<(), capnp::Error> {
            let client: action_catalogue_stream::Client =
                capnp_rpc::new_client(FakeActionCatalogueStream {
                    initial: RefCell::new(Some(
                        serde_json::json!({"version": 1, "surfaces": []}).to_string(),
                    )),
                });
            results.get().set_stream(client);
            Promise::ok(())
        }

        fn fetch_scrollback_for(
            self: Rc<Self>,
            params: channel_handle::FetchScrollbackForParams,
            mut results: channel_handle::FetchScrollbackForResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(p) = params.get() else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().scrollback_requests.push((
                p.get_artifact_id()
                    .map_or_else(|_| Vec::new(), <[u8]>::to_vec),
                p.get_before_seq(),
                p.get_max(),
            ));
            let seed = self
                .scrollback_response
                .borrow()
                .clone()
                .unwrap_or(SeedScrollback {
                    ops: Vec::new(),
                    earliest_seq: 0,
                    latest_seq: 0,
                    done: true,
                });
            let mut batch: scrollback_batch::Builder = results.get().init_batch();
            {
                let mut ops = batch
                    .reborrow()
                    .init_ops(u32::try_from(seed.ops.len()).unwrap_or(u32::MAX));
                for (i, op) in seed.ops.iter().enumerate() {
                    ops.set(u32::try_from(i).unwrap_or(u32::MAX), op);
                }
            }
            batch.set_earliest_seq(seed.earliest_seq);
            batch.set_latest_seq(seed.latest_seq);
            batch.set_done(seed.done);
            Promise::ok(())
        }
    }

    struct FakeBroker {
        captures: SharedCaptures,
        op_backlog: Vec<(Vec<u8>, String)>,
        presence_backlog: Vec<SeedPresence>,
        context_backlog: Vec<SeedContext>,
        scrollback_response: Option<SeedScrollback>,
    }

    impl collab_service::Server for FakeBroker {
        fn join_channel(
            self: Rc<Self>,
            params: collab_service::JoinChannelParams,
            mut results: collab_service::JoinChannelResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(reader) = params.get() else {
                return Promise::ok(());
            };
            let channel_id = reader
                .get_channel_id()
                .and_then(|t| {
                    t.to_str()
                        .map(str::to_owned)
                        .map_err(|e| capnp::Error::failed(format!("bad utf8: {e}")))
                })
                .unwrap_or_default();
            let identity = reader
                .get_identity()
                .map(<[u8]>::to_vec)
                .unwrap_or_default();
            self.captures
                .lock()
                .unwrap()
                .joined
                .push((channel_id, identity));

            let chan = FakeChannel {
                captures: self.captures.clone(),
                op_backlog: Rc::new(RefCell::new(self.op_backlog.clone())),
                presence_backlog: Rc::new(RefCell::new(self.presence_backlog.clone())),
                context_backlog: Rc::new(RefCell::new(self.context_backlog.clone())),
                scrollback_response: Rc::new(RefCell::new(self.scrollback_response.clone())),
            };
            let client: channel_handle::Client = capnp_rpc::new_client(chan);
            results.get().set_handle(client);
            Promise::ok(())
        }
    }

    /// Stand up a fake `CollabService` on one end of a Unix socket pair;
    /// returns the client-side raw fd + captures state for assertions.
    fn stand_up_fake_broker(broker: FakeBroker) -> (RawFd, SharedCaptures, thread::JoinHandle<()>) {
        let captures = broker.captures.clone();
        let (server_side, client_side) = std::os::unix::net::UnixStream::pair().unwrap();
        server_side.set_nonblocking(true).unwrap();
        client_side.set_nonblocking(true).unwrap();
        let client_fd = client_side.into_raw_fd();

        let handle = thread::Builder::new()
            .name("ui-collab-test-broker".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let local = TokioLocalSet::new();
                local.block_on(&rt, async move {
                    let tokio_stream =
                        TokioUnixStream::from_std(server_side).expect("wrap server side");
                    let broker_client: collab_service::Client = capnp_rpc::new_client(broker);
                    let (r, w) = tokio_stream.into_split();
                    let network = Box::new(twoparty::VatNetwork::new(
                        r.compat(),
                        w.compat_write(),
                        rpc_twoparty_capnp::Side::Server,
                        capnp::message::ReaderOptions::default(),
                    ));
                    let rpc = RpcSystem::new(network, Some(broker_client.client));
                    let _ = rpc.await;
                });
            })
            .unwrap();

        (client_fd, captures, handle)
    }

    fn drain_events(mgr: &mut CollaborationManager, deadline: Duration) -> Vec<CollabEvent> {
        let start = Instant::now();
        let mut events = Vec::new();
        while start.elapsed() < deadline {
            events.extend(mgr.poll());
            if !events.is_empty() {
                thread::sleep(Duration::from_millis(5));
                events.extend(mgr.poll());
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        events
    }

    fn wait_until<F: FnMut() -> bool>(mut cond: F, deadline: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if cond() {
                return true;
            }
            thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn saturated_outbound_bridge_sheds_only_presence_then_fails_closed() {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let document = || {
            SdkCommand::Primary(CollabRequest::DocumentIncremental {
                data: b"durable".to_vec(),
            })
        };
        let presence = || {
            SdkCommand::Primary(CollabRequest::Presence {
                key: PresenceKey::Tab {
                    id: "saturated".to_owned(),
                },
                payload: PresencePayload::Pointer { x: 1.0, y: 2.0 },
            })
        };

        assert!(enqueue_sdk_command(&tx, document(), &state));
        for _ in 0..10_000 {
            assert!(
                !enqueue_sdk_command(&tx, presence(), &state),
                "a full bounded queue may shed only this transient update"
            );
        }
        assert!(state.is_healthy());
        assert!(matches!(
            rx.try_recv(),
            Ok(QueuedSdkCommand {
                command: SdkCommand::Primary(CollabRequest::DocumentIncremental { data }),
                ..
            })
                if data == b"durable"
        ));

        assert!(enqueue_sdk_command(&tx, document(), &state));
        assert!(
            !enqueue_sdk_command(&tx, document(), &state),
            "a durable overflow must be observable as a disconnected bridge"
        );
        assert!(!state.is_healthy());
        assert!(
            rx.try_recv().is_ok() && rx.try_recv().is_err(),
            "the bounded queue retains only its admitted durable command"
        );
    }

    #[test]
    fn saturated_inbound_bridge_preserves_presence_policy_and_fails_for_results() {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let inbound = InboundBridge {
            tx,
            state: state.clone(),
        };

        for _ in 0..10_000 {
            assert!(inbound.send_presence(CollabEvent::RemoteCursorMoved {
                user_id: "peer".to_owned(),
                x: 1.0,
                y: 2.0,
            }));
        }
        assert!(state.is_healthy());
        assert!(matches!(
            rx.try_recv(),
            Ok(QueuedCollabEvent {
                event: CollabEvent::RemoteCursorMoved { .. },
                ..
            })
        ));

        assert!(inbound.send_durable(CollabEvent::ActionCatalogue {
            surfaces: Vec::new(),
        }));
        assert!(
            !inbound.send_durable(CollabEvent::PublishedInvocationResult {
                request_id: 9,
                result: Err("would have been lost".to_owned()),
            }),
            "an invocation lifecycle result is never silently discarded"
        );
        assert!(!state.is_healthy());
        assert!(matches!(
            rx.try_recv(),
            Ok(QueuedCollabEvent {
                event: CollabEvent::ActionCatalogue { surfaces },
                ..
            }) if surfaces.is_empty()
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn oversized_durable_payload_fails_the_byte_budget_before_queueing() {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let mut data = Vec::with_capacity(5 * 1024 * 1024);
        data.push(0);
        let oversized = SdkCommand::Primary(CollabRequest::DocumentSnapshot {
            // Capacity, not just logical length, is charged: a caller must
            // not park a mostly-empty giant allocation on the UI bridge.
            data,
        });

        assert!(!enqueue_sdk_command(&tx, oversized, &state));
        assert!(!state.is_healthy());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn byte_budget_rejects_aggregate_documents_before_count_capacity() {
        let (tx, rx) = tokio_mpsc::channel(SDK_COMMAND_QUEUE);
        let state = BridgeState::new();
        let mut admitted = 0;
        loop {
            let command = SdkCommand::Primary(CollabRequest::DocumentIncremental {
                data: vec![0; 64 * 1024],
            });
            if !enqueue_sdk_command(&tx, command, &state) {
                break;
            }
            admitted += 1;
        }

        assert!(admitted < SDK_COMMAND_QUEUE);
        assert!(!state.is_healthy());
        assert_eq!(rx.len(), admitted);
    }

    #[test]
    fn byte_reservation_releases_when_the_ui_consumes_a_command() {
        let (tx, mut rx) = tokio_mpsc::channel(2);
        let state = BridgeState::new();
        let document = || {
            SdkCommand::Primary(CollabRequest::DocumentIncremental {
                data: vec![0; 3 * 1024 * 1024],
            })
        };

        // Consuming its queued wrapper drops its reservation before the next
        // admission, rather than leaking the aggregate budget.
        assert!(enqueue_sdk_command(&tx, document(), &state));
        drop(rx.try_recv().expect("queued document"));
        assert!(enqueue_sdk_command(&tx, document(), &state));
        assert!(state.is_healthy());
    }

    #[test]
    fn oversized_inbound_action_fails_before_queueing() {
        let (tx, mut rx) = tokio_mpsc::channel(1);
        let state = BridgeState::new();
        let inbound = InboundBridge {
            tx,
            state: state.clone(),
        };
        let mut payload = Vec::with_capacity(5 * 1024 * 1024);
        payload.push(1);

        assert!(!inbound.send_durable(CollabEvent::ActionArrived {
            type_id: "test.action".to_owned(),
            payload,
            from_user_id: "peer".to_owned(),
        }));
        assert!(!state.is_healthy());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    #[serial_test::serial(qos_svc_collab_fd)]
    fn a_broker_that_is_not_there_does_not_hold_the_app_up() {
        // A connected socket whose far end is gone — a broker that died
        // between the launcher's connect and the app's first call. `new()`
        // runs before the first frame, so this must return and keep working.
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("socketpair");
        drop(theirs);
        let fd = ours.into_raw_fd();
        // SAFETY: serialised on `qos_svc_collab_fd` against every other
        // test that reads or writes these two variables.
        unsafe {
            std::env::set_var("QOS_SVC_COLLAB_FD", fd.to_string());
        }

        let started = Instant::now();
        let mut mgr = CollaborationManager::new();
        let took = started.elapsed();

        // SAFETY: as above.
        unsafe { std::env::remove_var("QOS_SVC_COLLAB_FD") };

        assert!(
            took < SHOVEL_INIT_TIMEOUT * 2,
            "constructing the manager must not stall the first frame (took {took:?})"
        );
        // Whichever backend it settled on, the app goes on calling these
        // every frame. Neither may panic.
        mgr.send(CollabRequest::DocumentSnapshot {
            data: b"after the broker died".to_vec(),
        });
        let _ = mgr.poll();
    }

    #[test]
    #[serial_test::serial(qos_svc_collab_fd)]
    fn disconnected_manager_is_a_no_op() {
        // No SDK, no legacy socket set — falls through to `Backend::Disabled`.
        // SAFETY: the crate is single-threaded per-test module here.
        unsafe {
            std::env::remove_var("QOS_SVC_COLLAB_FD");
        }
        let mut mgr = CollaborationManager::new();
        assert!(!mgr.is_connected(), "no service = not connected");
        mgr.send(CollabRequest::DocumentSnapshot {
            data: b"nope".to_vec(),
        });
        assert!(mgr.poll().is_empty(), "no events on Disabled backend");
    }

    #[test]
    fn explicit_test_join_forwards_its_identity_presentation() {
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let cluster = Uuid::new_v4();
        let presentation = b"certified-test-presentation".to_vec();
        let _mgr = CollaborationManager::from_sdk_fd_for_test_with_identity(
            fd,
            "alice",
            cluster,
            presentation.clone(),
        );

        assert!(
            wait_until(
                || !captures.lock().unwrap().joined.is_empty(),
                Duration::from_secs(2),
            ),
            "join_channel must be observed"
        );
        let joined = captures.lock().unwrap().joined.clone();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].1, presentation);
    }

    #[test]
    fn hello_joins_channel_and_send_forwards_signed_op() {
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let cluster = Uuid::new_v4();
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", cluster);

        assert!(
            wait_until(
                || !captures.lock().unwrap().joined.is_empty(),
                Duration::from_secs(2)
            ),
            "join_channel must be observed"
        );
        let joined = captures.lock().unwrap().joined.clone();
        assert_eq!(joined.len(), 1);
        // The channel id carries the whole address, app id included.
        assert_eq!(
            joined[0].0,
            channel_key::format("alice", &cluster.to_string(), ""),
        );

        let mut op = SignedOp::new_unsigned(
            b"payload".to_vec(),
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([1u8; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        );
        op.signature = collab_types::Signature::from_bytes([0u8; 64]);
        let expected_bytes = postcard::to_stdvec(&op).unwrap();
        mgr.send(CollabRequest::SignedOperation {
            envelope: Box::new(op),
        });

        assert!(
            wait_until(
                || !captures.lock().unwrap().signed_ops.is_empty(),
                Duration::from_secs(2),
            ),
            "send() must reach the SDK",
        );
        let seen = captures.lock().unwrap().signed_ops.clone();
        assert_eq!(seen, vec![expected_bytes]);
    }

    /// Every presence variant reaches the SDK method that keeps its key —
    /// a pointer publishes keyed, like a caret, rather than through
    /// `publish_cursor(x, y)`, which has nowhere to put one.
    #[test]
    fn presence_variants_map_to_correct_sdk_publishes() {
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        assert!(
            wait_until(
                || !captures.lock().unwrap().joined.is_empty(),
                Duration::from_secs(2)
            ),
            "join_channel must be observed",
        );

        let key = PresenceKey::Tab {
            id: "note-1".to_owned(),
        };
        let expected_key_bytes = postcard::to_stdvec(&key).unwrap();

        mgr.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Pointer { x: 1.5, y: 2.5 },
        });
        mgr.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Caret { pos: 12 },
        });
        mgr.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Selection { start: 3, end: 9 },
        });
        mgr.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Typing { start: 4, end: 10 },
        });
        mgr.send(CollabRequest::Presence {
            key: key.clone(),
            payload: PresencePayload::Arrived,
        });
        mgr.send(CollabRequest::Presence {
            key,
            payload: PresencePayload::Left,
        });

        assert!(
            wait_until(
                || {
                    let c = captures.lock().unwrap();
                    c.pointer_publishes.len() == 1
                        && c.caret_publishes.len() == 1
                        && c.selection_publishes.len() == 1
                        && c.typing_publishes.len() == 1
                        && c.arrived.len() == 1
                        && c.left.len() == 1
                },
                Duration::from_secs(2),
            ),
            "every presence variant must reach the corresponding SDK method",
        );
        let (
            pointer_publishes,
            cursor_publishes,
            caret_publishes,
            selection_publishes,
            typing_publishes,
            arrived,
            left,
        ) = {
            let c = captures.lock().unwrap();
            (
                c.pointer_publishes.clone(),
                c.cursor_publishes.clone(),
                c.caret_publishes.clone(),
                c.selection_publishes.clone(),
                c.typing_publishes.clone(),
                c.arrived.clone(),
                c.left.clone(),
            )
        };
        assert_eq!(
            pointer_publishes,
            vec![(expected_key_bytes.clone(), 1.5, 2.5)],
            "a pointer carries the key its publisher chose, like every other \
             keyed variant here",
        );
        assert!(
            cursor_publishes.is_empty(),
            "and does not go out through the keyless call, which would drop \
             the space on the way",
        );
        assert_eq!(caret_publishes, vec![(expected_key_bytes.clone(), 12)]);
        assert_eq!(
            selection_publishes,
            vec![(expected_key_bytes.clone(), 3, 9)]
        );
        assert_eq!(typing_publishes, vec![(expected_key_bytes.clone(), 4, 10)]);
        assert_eq!(arrived, vec![expected_key_bytes.clone()]);
        assert_eq!(left, vec![expected_key_bytes]);
    }

    #[test]
    fn signing_identity_refuses_sequence_reuse_at_u64_boundary() {
        let secret_key = ed25519_dalek::SigningKey::from_bytes(&[7_u8; 32]);
        let mut identity = SigningIdentity::new(secret_key, collab_types::AttestationId::new());
        identity.seq = u64::MAX - 1;
        let artifact = collab_types::ArtifactId::new();

        let final_op = identity
            .sign_op(
                b"last unique operation".to_vec(),
                artifact,
                collab_types::OpClass::Splice,
            )
            .expect("the final unique sequence remains valid");
        assert_eq!(final_op.seq, u64::MAX);
        assert_eq!(identity.seq, u64::MAX);

        let exhausted = identity.sign_op(
            b"must not reuse zero".to_vec(),
            artifact,
            collab_types::OpClass::Splice,
        );
        assert!(matches!(exhausted, Err(SignedSendError::SequenceExhausted)));
        assert_eq!(identity.seq, u64::MAX, "failure is non-mutating");
    }

    #[test]
    fn signed_op_stream_translates_to_signed_crdt_incremental() {
        let mut op = SignedOp::new_unsigned(
            collab_types::DocumentMutation::Incremental(b"remote-payload".to_vec())
                .encode()
                .unwrap(),
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([2u8; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            42,
        );
        op.signature = collab_types::Signature::from_bytes([0u8; 64]);
        let op_bytes = postcard::to_stdvec(&op).unwrap();

        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![(op_bytes, "peer-bob".to_owned())],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        let events = drain_events(&mut mgr, Duration::from_secs(2));

        let signed: Vec<_> = events
            .into_iter()
            .filter_map(|e| match e {
                CollabEvent::Document {
                    envelope,
                    from_user_id,
                    ..
                } => Some((envelope, from_user_id)),
                _ => None,
            })
            .collect();
        assert_eq!(signed.len(), 1, "one document event expected");
        assert_eq!(signed[0].1, "peer-bob");
        assert_eq!(
            collab_types::DocumentMutation::decode(&signed[0].0.payload).unwrap(),
            collab_types::DocumentMutation::Incremental(b"remote-payload".to_vec())
        );
        assert_eq!(signed[0].0.seq, 42);
    }

    #[test]
    fn signed_document_event_preserves_snapshot_kind() {
        let op = SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(b"whole document".to_vec())
                .encode()
                .unwrap(),
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([3_u8; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            43,
        );
        let event = signed_document_event(
            op,
            "peer-alice".into(),
            &std::sync::Arc::new(std::sync::Mutex::new(None)),
        )
        .unwrap();
        assert!(matches!(
            &event,
            CollabEvent::Document { from_user_id, .. } if from_user_id == "peer-alice"
        ));
        assert!(matches!(
            event.document_mutation(),
            Some(Ok(DocumentMutation::Snapshot(data))) if data == b"whole document"
        ));
    }

    #[test]
    fn signed_document_event_decrypts_at_endpoint_and_rejects_replay() {
        let channel = collab_types::ChannelId::new();
        let alice_key = channel_crypto::MemberKey::generate("alice").unwrap();
        let bob_key = channel_crypto::MemberKey::generate("bob").unwrap();
        let bob_package = bob_key.fresh_key_package().unwrap();
        let mut alice = channel_crypto::ChannelGroup::create(channel, alice_key).unwrap();
        let welcome = alice.add_member(bob_package).unwrap();
        let bob =
            channel_crypto::ChannelGroup::process_welcome(channel, &welcome, bob_key).unwrap();
        let plaintext = DocumentMutation::Incremental(b"secret change".to_vec())
            .encode()
            .unwrap();
        let alice_endpoint = std::sync::Arc::new(std::sync::Mutex::new(Some(alice)));
        let (ciphertext, metadata) =
            prepare_document_payload(&alice_endpoint, plaintext.clone()).unwrap();
        let metadata = metadata.expect("a provisioned endpoint never sends plaintext");
        assert_eq!(metadata.channel, channel);
        let op = SignedOp::new_unsigned_with_encryption(
            ciphertext,
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([9; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            1,
            metadata,
        );
        let endpoint = std::sync::Arc::new(std::sync::Mutex::new(Some(bob)));
        let event = signed_document_event(op.clone(), "alice".into(), &endpoint).unwrap();
        assert!(matches!(
            event.document_mutation(),
            Some(Ok(DocumentMutation::Incremental(data))) if data == b"secret change"
        ));
        let plaintext_downgrade = SignedOp::new_unsigned(
            plaintext,
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([9; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            2,
        );
        assert!(
            signed_document_event(plaintext_downgrade, "alice".into(), &endpoint).is_err(),
            "a provisioned MLS route must reject plaintext downgrade"
        );
        assert!(
            signed_document_event(op, "alice".into(), &endpoint).is_err(),
            "the MLS receive ratchet must reject replaying one ciphertext"
        );
    }

    #[test]
    fn context_stream_translates_to_context_event() {
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![SeedContext {
                collaborators: vec![
                    ("alice".to_owned(), "Alice".to_owned(), 0x00FF_8000),
                    ("bob".to_owned(), "Bob".to_owned(), 0x0000_80FF),
                ],
                permissions: vec!["edit".to_owned(), "share".to_owned()],
            }],
            scrollback_response: None,
        };
        let (fd, _captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        let events = drain_events(&mut mgr, Duration::from_secs(2));

        let ctx = events
            .iter()
            .find_map(|e| match e {
                CollabEvent::Context {
                    collaborators,
                    permissions,
                } => Some((collaborators.clone(), permissions.clone())),
                _ => None,
            })
            .expect("must receive a Context event");
        let (collabs, perms) = ctx;
        assert_eq!(collabs.len(), 2);
        assert_eq!(collabs[0].id, "alice");
        assert_eq!(collabs[0].name, "Alice");
        // 0x00FF8000 → R=255, G=128, B=0.
        assert!((collabs[0].color[0] - 1.0).abs() < 1e-3);
        assert!((collabs[0].color[1] - 128.0 / 255.0).abs() < 1e-3);
        assert!(collabs[0].color[2].abs() < 1e-3);
        assert_eq!(perms, vec!["edit".to_owned(), "share".to_owned()]);
    }

    #[test]
    fn presence_stream_translates_cursor_and_all_other_variants() {
        let key = PresenceKey::Tab {
            id: "note-1".to_owned(),
        };
        let key_bytes = postcard::to_stdvec(&key).unwrap();
        let presence_backlog = vec![
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes: key_bytes.clone(),
                payload: SeedPayload::Cursor { x: 4.0, y: 5.0 },
            },
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes: key_bytes.clone(),
                payload: SeedPayload::Pointer { x: 6.0, y: 9.0 },
            },
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes: key_bytes.clone(),
                payload: SeedPayload::Caret { pos: 7 },
            },
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes: key_bytes.clone(),
                payload: SeedPayload::Selection { start: 1, end: 3 },
            },
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes: key_bytes.clone(),
                payload: SeedPayload::Typing { start: 2, end: 5 },
            },
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes: key_bytes.clone(),
                payload: SeedPayload::Arrived,
            },
            SeedPresence {
                user_id: "peer-bob".to_owned(),
                key_bytes,
                payload: SeedPayload::Left,
            },
        ];
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog,
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        let start = Instant::now();
        let mut events = Vec::new();
        while events.len() < 7 && start.elapsed() < Duration::from_secs(2) {
            events.extend(mgr.poll());
            thread::sleep(Duration::from_millis(5));
        }

        let mut cursor_moves = Vec::new();
        let mut updates = Vec::new();
        for e in events {
            match e {
                CollabEvent::RemoteCursorMoved { x, y, .. } => cursor_moves.push((x, y)),
                CollabEvent::RemotePresenceUpdate { key, payload, .. } => {
                    updates.push((key, payload));
                }
                _ => {}
            }
        }
        assert!(
            cursor_moves.contains(&(4.0, 5.0)),
            "cursor payload must land on RemoteCursorMoved"
        );
        assert!(
            updates.contains(&(key.clone(), PresencePayload::Pointer { x: 6.0, y: 9.0 })),
            "keyed pointer retains its coordinate-space key"
        );
        assert!(
            updates.contains(&(key.clone(), PresencePayload::Caret { pos: 7 })),
            "caret variant translated"
        );
        assert!(
            updates.contains(&(key.clone(), PresencePayload::Selection { start: 1, end: 3 })),
            "selection variant translated"
        );
        assert!(
            updates.contains(&(key.clone(), PresencePayload::Typing { start: 2, end: 5 })),
            "typing remains distinct from selection"
        );
        assert!(
            updates.contains(&(key.clone(), PresencePayload::Arrived)),
            "arrived variant translated"
        );
        assert!(
            updates.contains(&(key, PresencePayload::Left)),
            "left variant translated"
        );
    }

    #[test]
    fn fetch_scrollback_returns_batch_event() {
        let payload = collab_types::DocumentMutation::Incremental(b"old-payload".to_vec())
            .encode()
            .unwrap();
        let mut op = SignedOp::new_unsigned(
            payload,
            collab_types::ArtifactId::new(),
            collab_types::Ed25519PublicKey::from_bytes([3u8; 32]),
            collab_types::AttestationId::new(),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            5,
        );
        op.signature = collab_types::Signature::from_bytes([0u8; 64]);
        let op_bytes = postcard::to_stdvec(&op).unwrap();
        let seed = SeedScrollback {
            ops: vec![op_bytes],
            earliest_seq: 5,
            latest_seq: 5,
            done: true,
        };
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: Some(seed),
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let mut mgr = CollaborationManager::from_sdk_fd_for_test(fd, "alice", Uuid::nil());
        assert!(
            wait_until(
                || !captures.lock().unwrap().joined.is_empty(),
                Duration::from_secs(2)
            ),
            "join_channel must precede scrollback fetch",
        );

        let artifact = collab_types::ArtifactId::new();
        mgr.send(CollabRequest::FetchScrollback {
            artifact,
            before_seq: 10,
            max: 50,
        });
        let events = drain_events(&mut mgr, Duration::from_secs(2));
        let batch = events
            .into_iter()
            .find_map(|e| match e {
                CollabEvent::ScrollbackBatch { ops, exhausted, .. } => Some((ops, exhausted)),
                _ => None,
            })
            .expect("must receive a ScrollbackBatch event");
        assert_eq!(batch.0.len(), 1);
        assert_eq!(
            collab_types::DocumentMutation::decode(&batch.0[0].payload).unwrap(),
            collab_types::DocumentMutation::Incremental(b"old-payload".to_vec())
        );
        assert!(batch.1, "seeded broker reports exhausted");
        assert_eq!(
            captures.lock().unwrap().scrollback_requests,
            vec![(artifact.0.as_bytes().to_vec(), 10, 50)],
            "artifact / before_seq / max round-tripped verbatim",
        );
    }

    #[test]
    fn translate_presence_event_is_symmetric_over_all_payload_variants() {
        // Straight unit test of `translate_presence_event` — no broker or
        // threading needed. Malformed scopes are deliberately not projected.
        let key = PresenceKey::Widget {
            path: vec![crate::presence::PathSegment::Field("body".into())],
        };
        let key_bytes = postcard::to_stdvec(&key).unwrap();

        let e = translate_presence_event(runtime_collab::PresenceEvent {
            user_id: "peer".into(),
            key_bytes: key_bytes.clone(),
            payload: SdkPayload::Cursor { x: 7.0, y: 8.0 },
        })
        .expect("cursor event");
        matches!(e, CollabEvent::RemoteCursorMoved { .. });

        let e = translate_presence_event(runtime_collab::PresenceEvent {
            user_id: "peer".into(),
            key_bytes,
            payload: SdkPayload::Caret { pos: 3 },
        })
        .expect("caret event");
        if let CollabEvent::RemotePresenceUpdate {
            key: k, payload, ..
        } = e
        {
            assert_eq!(k, key);
            assert_eq!(payload, PresencePayload::Caret { pos: 3 });
        } else {
            panic!("expected RemotePresenceUpdate");
        }

        let e = translate_presence_event(runtime_collab::PresenceEvent {
            user_id: "peer".into(),
            key_bytes: vec![0xFF, 0xFF, 0xFF],
            payload: SdkPayload::Arrived,
        });
        assert!(
            e.is_none(),
            "an undecodable target is not semantic presence"
        );
    }

    #[test]
    fn invocation_context_json_is_optional_and_fail_closed() {
        let context = qos_actions::ActionContext::default()
            .with_purpose("Review the proposal")
            .with_group("task:review", "Review", "task");
        let json = serde_json::to_string(&context).unwrap();
        assert_eq!(parse_invocation_context(7, &json), Some(context));
        assert_eq!(parse_invocation_context(8, "  "), None);
        assert_eq!(parse_invocation_context(9, "{not-json"), None);
    }
}
