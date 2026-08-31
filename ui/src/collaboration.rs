//! Toolkit-side collab client: a synchronous, non-blocking `send` / `poll`
//! pair trading typed [`CollabRequest`] / [`CollabEvent`] frames, stable so
//! every downstream app keeps compiling while the transport underneath
//! changes.
//!
//! The manager delegates to the `runtime-collab` Cap'n Proto SDK on a
//! shovel thread it owns, bridging winit-side `send` / `poll` to the SDK
//! over a pair of unbounded mpsc channels. On
//! [`runtime_portal::NotAvailable`] it enters "disabled" mode instead of
//! panicking — `poll()` returns empty, `send()` is a no-op, no thread is
//! spawned.
//!

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use collab_types::SignedOp;

use crate::presence::PresenceKey;

use std::os::fd::RawFd;
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabRequest {
    Hello {
        app_id: String,
        cluster_id: Uuid,
        /// Which **record** ([`crate::record`]) this connection carries, when
        /// adopted. The broker delivers payloads only to connections
        /// announcing the same record, so two people editing different
        /// files in one room don't merge each other's state; `None` means
        /// one document per `(app, room)`. `#[serde(default)]` so a frame
        /// from a pre-record peer still parses as `None` instead of being
        /// dropped.
        #[serde(default)]
        record: Option<String>,
        /// Name on the caret pill; `None` gets a synthetic `peer-<n>`.
        /// Informational only — never used for routing or grants.
        #[serde(default)]
        handle: Option<String>,
    },
    /// Legacy: unsigned full-document state. Apps using this path do not
    /// get role enforcement and surface a "legacy / unsigned" badge in
    /// chrome.
    SyncCRDT { data: Vec<u8> },
    /// Legacy: unsigned incremental Automerge change blob. New code
    /// should prefer [`CollabRequest::SignedSyncIncremental`].
    SyncIncremental { data: Vec<u8> },
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
    SignedSyncIncremental { envelope: Box<SignedOp> },
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabEvent {
    Context {
        collaborators: Vec<Collaborator>,
        permissions: Vec<String>,
    },
    RemoteCursorMoved {
        user_id: String,
        x: f32,
        y: f32,
    },
    /// Legacy: full unsigned document state, merged into the local
    /// Automerge doc.
    CRDTUpdate {
        data: Vec<u8>,
    },
    /// Legacy: unsigned incremental change blob. See
    /// [`CollabRequest::SyncIncremental`].
    CRDTIncremental {
        data: Vec<u8>,
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
    /// A validated signed envelope from another peer (validation ran on the
    /// broker). Receivers extract `envelope.payload` into the same
    /// `automerge::apply_changes` path [`CollabEvent::CRDTIncremental`] uses.
    ///
    /// `from_user_id` mirrors the broker's synthetic id
    /// (`"peer-<src_id>"`) so attribution code uses one id shape.
    SignedCRDTIncremental {
        envelope: Box<SignedOp>,
        from_user_id: String,
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
        exhausted: bool,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
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
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    pub color: [f32; 3],
    /// Defaulted on the wire so a peer that omits the field reads as a
    /// person — a smaller lie than a roster that won't load.
    #[serde(default)]
    pub is_agent: bool,
    /// Whether this row is the receiving machine's own participant.
    /// Apps can't derive this themselves: `id` is the address *other*
    /// machines use, and a machine never announces its own to its own apps.
    ///
    /// Defaulted on the wire, so a peer that predates this reads as someone
    /// else.
    #[serde(default)]
    pub is_self: bool,
}

/// Failure modes for [`CollaborationManager::send_signed_op`].
/// `NoIdentity` is the common case — call before the shell provisions an
/// identity — and callers can fall back to plain `SyncIncremental`.
#[derive(Debug)]
pub enum SignedSendError {
    /// No [`SigningIdentity`] bound (see [`CollaborationManager::with_identity`]).
    NoIdentity,
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
    /// Returns [`collab_types::CanonicalError`] when `payload` fails to
    /// canonicalize for signing.
    pub fn sign_op(
        &mut self,
        payload: Vec<u8>,
        artifact: collab_types::ArtifactId,
        op_class: collab_types::OpClass,
    ) -> Result<collab_types::SignedOp, collab_types::CanonicalError> {
        let timestamp = std::time::SystemTime::now();
        self.seq = self.seq.wrapping_add(1);
        let mut op = collab_types::SignedOp::new_unsigned(
            payload,
            artifact,
            self.public_key,
            self.attestation_id,
            op_class,
            timestamp,
            self.seq,
        );
        collab_types::sign_op(&mut op, &self.secret_key)?;
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
    /// through unbounded mpsc pairs.
    Sdk(SdkChannel),
    /// Routes co-edit deltas + presence through the workspace host via a
    /// [`qos_relay::RelayCoeditSession`], bridged like [`Backend::Sdk`].
    /// Deltas inherit the host's on-disk store-and-forward, so an offline
    /// peer still converges.
    Relay(RelayChannel),
}

/// Owned mpsc pair + shovel-thread handle for SDK mode. Both directions
/// are unbounded — the winit side drains fully each non-blocking
/// `poll()`, the tokio side always awaits `.recv()` — so neither has an
/// upper bound in normal operation.
///
/// Dropping the manager drops `outbound_tx`; the shovel thread's
/// `.recv()` then yields `None` and the thread exits. `_thread` is kept
/// only so the `JoinHandle` isn't detached.
struct SdkChannel {
    outbound_tx: tokio_mpsc::UnboundedSender<SdkCommand>,
    inbound_rx: tokio_mpsc::UnboundedReceiver<CollabEvent>,
    _thread: Option<thread::JoinHandle<()>>,
}

enum SdkCommand {
    Primary(CollabRequest),
    Join(RouteSpec),
    ToRoute(RouteSpec, CollabRequest),
}

/// Owned mpsc pair + bridge-task handle for Relay mode. Unlike
/// [`SdkChannel`] there's no dedicated OS thread — the session runs on
/// the caller's tokio runtime, so the bridge task is `spawn`ed there.
/// Dropping the manager closes `outbound_tx`, which the bridge's
/// `select!` observes; [`AbortOnDrop`] is the backstop.
struct RelayChannel {
    outbound_tx: tokio_mpsc::UnboundedSender<CollabRequest>,
    inbound_rx: tokio_mpsc::UnboundedReceiver<CollabEvent>,
    _task: AbortOnDrop,
}

/// Aborts the wrapped tokio task when dropped, so a dropped manager
/// never leaves its relay bridge task running against a session whose
/// far end is gone.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
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

pub struct CollaborationManager {
    backend: Backend,
    /// When `Some`, [`Self::send_signed_op`] mints a signed envelope and
    /// dispatches [`CollabRequest::SignedSyncIncremental`]. `None` is the
    /// legacy path: unsigned frames, fanned out unvalidated.
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
        )
    }

    /// Explicit typed endpoint for diagnostics and headless harnesses that
    /// are not launched through the app launcher. Takes ownership of `fd`.
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
        )
    }

    fn for_route_from(
        source: SdkConnectSource,
        app_id: String,
        cluster_id: Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self, runtime_portal::NotAvailable> {
        let sdk = Self::spawn_sdk(source, Vec::new())?;
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
        Ok(manager)
    }

    /// Connects through `runtime_collab::try_connect()`, entering SDK mode on
    /// `Ok` and Disabled mode on `Err`.
    ///
    /// Both modes auto-send `Hello` announcing **no record** (one document
    /// per `(app, room)`); use [`Self::for_record`] for apps that have
    /// adopted records.
    pub fn new() -> Self {
        Self::new_scoped(None)
    }
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
        // Pubkey bytes ride along on the join_channel call below, so presence
        // is authored on connect. `ServiceFdMissing` (not a qOS session) is
        // expected and quiet; any other failure gets a debug line so
        // `RUST_LOG=ui=debug` can answer "why does presence look anonymous?".
        let runtime_identity = match runtime_identity::load() {
            Ok(id) => Some(id),
            Err(runtime_portal::NotAvailable::ServiceFdMissing) => {
                tracing::debug!(
                    "runtime-identity: env var unset — collab join will use empty identity bytes"
                );
                None
            }
            Err(e) => {
                tracing::debug!(
                    "runtime-identity: load failed ({e:?}); collab join will use empty identity bytes"
                );
                None
            }
        };
        let identity_bytes = runtime_identity
            .map(|id| id.group_public_key().to_vec())
            .unwrap_or_default();

        match Self::spawn_sdk(SdkConnectSource::Env, identity_bytes) {
            Ok(sdk) => {
                let mut manager = Self {
                    backend: Backend::Sdk(sdk),
                    identity: None,
                };
                let (app_id, cluster_id) = auto_hello_env();
                manager.send(CollabRequest::Hello {
                    app_id,
                    cluster_id,
                    record,
                    handle: presence_handle_env(),
                });
                manager
            }
            Err(_) => Self {
                backend: Backend::Disabled,
                identity: None,
            },
        }
    }

    /// Route this manager's co-edit + presence through an already-joined
    /// [`qos_relay::RelayCoeditSession`], behind the same `send`/`poll`
    /// surface. Deltas travel through the host's fan-out **and** on-disk
    /// store-and-forward, so an offline peer still receives them on
    /// reconnect.
    ///
    /// The manager owns no signing identity in this mode — ops must be
    /// signed by the app before `send`; presence attribution comes from the
    /// session's own device key. `handle` is the runtime the bridge task
    /// spawns onto.
    pub fn from_relay_session(
        session: qos_relay::RelayCoeditSession,
        handle: &tokio::runtime::Handle,
    ) -> Self {
        let (outbound_tx, outbound_rx) = tokio_mpsc::unbounded_channel::<CollabRequest>();
        let (inbound_tx, inbound_rx) = tokio_mpsc::unbounded_channel::<CollabEvent>();
        let task = handle.spawn(drive_relay(session, outbound_rx, inbound_tx));
        Self {
            backend: Backend::Relay(RelayChannel {
                outbound_tx,
                inbound_rx,
                _task: AbortOnDrop(task),
            }),
            identity: None,
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
        let sdk = Self::spawn_sdk(SdkConnectSource::Fd(fd), identity_bytes)
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
    }

    /// Stand up the shovel thread + its tokio `current_thread` runtime.
    /// Runs `try_connect` **inside** the thread's `LocalSet` because
    /// capnp-rpc futures are `!Send` and can't cross a thread boundary.
    /// The result surfaces via [`InitStatus`] on a one-shot mpsc so the
    /// caller picks Disabled vs Sdk before returning, instead of an app
    /// that only ever `poll`s silently running against a dead broker.
    fn spawn_sdk(
        source: SdkConnectSource,
        identity_bytes: Vec<u8>,
    ) -> Result<SdkChannel, runtime_portal::NotAvailable> {
        let (outbound_tx, outbound_rx) = tokio_mpsc::unbounded_channel::<SdkCommand>();
        let (inbound_tx, inbound_rx) = tokio_mpsc::unbounded_channel::<CollabEvent>();
        let (init_tx, init_rx) = mpsc::sync_channel::<InitStatus>(1);

        // A thread we can't start is a service we can't reach — map to
        // NotAvailable rather than panicking; an app must open even without collab.
        let handle = match thread::Builder::new()
            .name("ui-collab-shovel".into())
            .spawn(move || run_shovel(source, identity_bytes, outbound_rx, inbound_tx, init_tx))
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
        !matches!(self.backend, Backend::Disabled)
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
    /// bound, or [`SignedSendError::Canonical`] when `payload` fails to
    /// canonicalize for signing.
    pub fn send_signed_op(
        &mut self,
        payload: Vec<u8>,
        artifact: collab_types::ArtifactId,
        op_class: collab_types::OpClass,
    ) -> Result<(), SignedSendError> {
        let identity = self.identity.as_mut().ok_or(SignedSendError::NoIdentity)?;
        let envelope = identity
            .sign_op(payload, artifact, op_class)
            .map_err(SignedSendError::Canonical)?;
        self.send(CollabRequest::SignedSyncIncremental {
            envelope: Box::new(envelope),
        });
        Ok(())
    }

    /// Fire-and-forget, non-blocking on every backend. Delivery failures
    /// never surface — apps needing confirmation build on `poll` events.
    pub fn send(&mut self, req: CollabRequest) {
        match &mut self.backend {
            Backend::Disabled => {}
            Backend::Sdk(ch) => {
                // Only errors when the shovel thread has exited; the
                // next poll observes the same shutdown.
                let _ = ch.outbound_tx.send(SdkCommand::Primary(req));
            }
            Backend::Relay(ch) => {
                let _ = ch.outbound_tx.send(req);
            }
        }
    }

    /// Join and subscribe to an additional route without publishing traffic.
    /// Repeated joins of the same route are idempotent.
    pub fn join_route(&mut self, route: RouteSpec) {
        if let Backend::Sdk(ch) = &mut self.backend {
            let _ = ch.outbound_tx.send(SdkCommand::Join(route));
        }
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
            let _ = ch.outbound_tx.send(SdkCommand::ToRoute(route, req));
        }
    }

    /// Drain every event pending on the current backend. Non-blocking.
    pub fn poll(&mut self) -> Vec<CollabEvent> {
        match &mut self.backend {
            Backend::Disabled => Vec::new(),
            Backend::Sdk(ch) => {
                let mut events = Vec::new();
                while let Ok(evt) = ch.inbound_rx.try_recv() {
                    events.push(evt);
                }
                events
            }
            Backend::Relay(ch) => {
                let mut events = Vec::new();
                while let Ok(evt) = ch.inbound_rx.try_recv() {
                    events.push(evt);
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
    if let Some(id) = runtime_identity::load()
        .ok()
        .or_else(|| config_identity_path().and_then(|p| runtime_identity::load_from_path(p).ok()))
    {
        return person_id_from_key(id.group_public_key());
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
    match crate::permissions::Principal::person_from_key(
        collab_types::crypto::Ed25519PublicKey::from_bytes(key),
    ) {
        crate::permissions::Principal::Person(id) => id,
        // `person_from_key` constructs `Person` unconditionally; this arm
        // exists only because the return type is the whole enum.
        _ => unreachable!("person_from_key returns a person"),
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
    identity_bytes: Vec<u8>,
    outbound_rx: tokio_mpsc::UnboundedReceiver<SdkCommand>,
    inbound_tx: tokio_mpsc::UnboundedSender<CollabEvent>,
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
        // Ready. Manager may now hand out `send` / `poll`.
        let _ = init_tx.send(InitStatus::Ready);

        drive_shovel(client, identity_bytes, outbound_rx, inbound_tx).await;
    });
}

/// SDK-side event loop: dispatches outbound requests to SDK calls, and
/// spawns one subscription task per stream on the first `join_channel`.
// Not `Send`: `client` wraps a capnp-rpc handle (`Rc`-based, `!Send`), and
// this only ever runs inside `run_shovel`'s `LocalSet` on its own thread.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn drive_shovel(
    client: runtime_collab::CollabClient,
    identity_bytes: Vec<u8>,
    mut outbound_rx: tokio_mpsc::UnboundedReceiver<SdkCommand>,
    inbound_tx: tokio_mpsc::UnboundedSender<CollabEvent>,
) {
    let mut routes = std::collections::HashMap::<String, SdkRoute>::new();
    let mut primary: Option<String> = None;
    let mut participant_handle: Option<String> = None;

    while let Some(command) = outbound_rx.recv().await {
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
                ensure_sdk_route(
                    &client,
                    &identity_bytes,
                    participant_handle.as_deref(),
                    &inbound_tx,
                    &mut routes,
                    &spec,
                )
                .await;
                primary = Some(key);
            }
            SdkCommand::Primary(request) => {
                let Some(key) = primary.as_ref() else {
                    tracing::warn!("collab: request sent before the primary route joined");
                    continue;
                };
                if let Some(route) = routes.get(key) {
                    dispatch_sdk_request(route, request, &inbound_tx).await;
                }
            }
            SdkCommand::Join(spec) => {
                ensure_sdk_route(
                    &client,
                    &identity_bytes,
                    participant_handle.as_deref(),
                    &inbound_tx,
                    &mut routes,
                    &spec,
                )
                .await;
            }
            SdkCommand::ToRoute(spec, request) => {
                let key = spec.channel_id();
                ensure_sdk_route(
                    &client,
                    &identity_bytes,
                    participant_handle.as_deref(),
                    &inbound_tx,
                    &mut routes,
                    &spec,
                )
                .await;
                if let Some(route) = routes.get(&key) {
                    dispatch_sdk_request(route, request, &inbound_tx).await;
                }
            }
        }
    }
}

struct SdkRoute {
    channel: runtime_collab::ChannelHandle,
    presence: Option<runtime_collab::PresenceHandle>,
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
    inbound_tx: &tokio_mpsc::UnboundedSender<CollabEvent>,
    routes: &mut std::collections::HashMap<String, SdkRoute>,
    spec: &RouteSpec,
) {
    let channel_id = spec.channel_id();
    if routes.contains_key(&channel_id) {
        return;
    }
    let channel = match client
        .join_channel_as(
            &channel_id,
            identity_bytes,
            participant_handle.unwrap_or(""),
        )
        .await
    {
        Ok(channel) => channel,
        Err(error) => {
            tracing::warn!(
                "collab: joining channel {channel_id} failed ({error}); route unavailable"
            );
            return;
        }
    };

    spawn_op_stream(&channel, inbound_tx).await;
    spawn_context_stream(&channel, inbound_tx).await;
    spawn_action_stream(&channel, inbound_tx).await;
    let presence = spawn_presence_stream(&channel, inbound_tx).await;

    routes.insert(channel_id, SdkRoute { channel, presence });
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
    inbound_tx: &tokio_mpsc::UnboundedSender<CollabEvent>,
) {
    if let Ok(stream) = channel.subscribe().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(event) = stream.read().await {
                let translated = match event.kind {
                    runtime_collab::OpKind::Signed => {
                        match postcard::from_bytes::<SignedOp>(&event.op) {
                            Ok(envelope) => CollabEvent::SignedCRDTIncremental {
                                envelope: Box::new(envelope),
                                from_user_id: event.from_user_id,
                            },
                            Err(error) => {
                                tracing::warn!(
                                    "collab: signed op is not postcard SignedOp ({error}); dropping"
                                );
                                continue;
                            }
                        }
                    }
                    runtime_collab::OpKind::UnsignedIncremental => {
                        CollabEvent::CRDTIncremental { data: event.op }
                    }
                    runtime_collab::OpKind::UnsignedSnapshot => {
                        CollabEvent::CRDTUpdate { data: event.op }
                    }
                };
                if tx.send(translated).is_err() {
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
async fn spawn_context_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &tokio_mpsc::UnboundedSender<CollabEvent>,
) {
    if let Ok(stream) = channel.subscribe_context().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(context) = stream.read().await {
                let collaborators = context
                    .collaborators
                    .into_iter()
                    .map(|collaborator| Collaborator {
                        id: collaborator.user_id,
                        name: collaborator.display_name,
                        color: unpack_color(collaborator.color),
                        is_agent: false,
                        is_self: false,
                    })
                    .collect();
                if tx
                    .send(CollabEvent::Context {
                        collaborators,
                        permissions: context.permissions,
                    })
                    .is_err()
                {
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
async fn spawn_action_stream(
    channel: &runtime_collab::ChannelHandle,
    inbound_tx: &tokio_mpsc::UnboundedSender<CollabEvent>,
) {
    if let Ok(stream) = channel.subscribe_actions().await {
        let tx = inbound_tx.clone();
        tokio::task::spawn_local(async move {
            while let Ok(action) = stream.read().await {
                if tx
                    .send(CollabEvent::ActionArrived {
                        type_id: action.type_id,
                        payload: action.payload,
                        from_user_id: action.from_user_id,
                    })
                    .is_err()
                {
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
    inbound_tx: &tokio_mpsc::UnboundedSender<CollabEvent>,
) -> Option<runtime_collab::PresenceHandle> {
    match channel.presence().await {
        Ok(handle) => {
            if let Ok(stream) = handle.subscribe_all().await {
                let tx = inbound_tx.clone();
                tokio::task::spawn_local(async move {
                    while let Ok(event) = stream.read().await {
                        if tx.send(translate_presence_event(event)).is_err() {
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

// Not `Send`: `route.channel` wraps a capnp-rpc handle (`Rc`-based,
// `!Send`), and this only ever runs inside the shovel thread's `LocalSet`.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc futures are !Send by design; driven only on the shovel thread's LocalSet"
)]
async fn dispatch_sdk_request(
    route: &SdkRoute,
    request: CollabRequest,
    inbound_tx: &tokio_mpsc::UnboundedSender<CollabEvent>,
) {
    match request {
        CollabRequest::Hello { .. } => {}
        CollabRequest::SignedSyncIncremental { envelope } => {
            if let Ok(bytes) = postcard::to_stdvec(&*envelope) {
                let _ = route.channel.send(&bytes).await;
            }
        }
        CollabRequest::SyncCRDT { data } => {
            let _ = route.channel.send_unsigned_snapshot(&data).await;
        }
        CollabRequest::SyncIncremental { data } => {
            let _ = route.channel.send_unsigned_incremental(&data).await;
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
                PresencePayload::Selection { start, end }
                | PresencePayload::Typing { start, end } => {
                    let _ = presence
                        .publish_selection(&key_bytes, start as u64, end as u64)
                        .await;
                }
                PresencePayload::Arrived => {
                    let _ = presence.publish_arrived(&key_bytes).await;
                }
                PresencePayload::Left => {
                    let _ = presence.publish_left(&key_bytes).await;
                }
            }
        }
        CollabRequest::FetchScrollback {
            artifact,
            before_seq,
            max,
        } => {
            let (ops, exhausted) = match route
                .channel
                .fetch_scrollback(before_seq, u32::from(max))
                .await
            {
                Ok(batch) => (
                    batch
                        .ops
                        .into_iter()
                        .filter_map(|bytes| postcard::from_bytes::<SignedOp>(&bytes).ok())
                        .map(Box::new)
                        .collect(),
                    batch.done,
                ),
                Err(_) => (Vec::new(), true),
            };
            let _ = inbound_tx.send(CollabEvent::ScrollbackBatch {
                artifact,
                ops,
                exhausted,
            });
        }
    }
}
/// Bridge task for [`Backend::Relay`]: shuttles between the toolkit's
/// synchronous `send`/`poll` and the session's async publish/recv —
/// outbound [`CollabRequest`]s map onto `publish_*`, inbound
/// [`qos_relay::Inbound`] map onto [`CollabEvent`].
///
/// The two directions share a `select!` since both borrow the session
/// immutably. Returns on manager drop or host-closed session.
async fn drive_relay(
    session: qos_relay::RelayCoeditSession,
    mut outbound_rx: tokio_mpsc::UnboundedReceiver<CollabRequest>,
    inbound_tx: tokio_mpsc::UnboundedSender<CollabEvent>,
) {
    loop {
        tokio::select! {
            biased;
            outbound = outbound_rx.recv() => {
                let Some(req) = outbound else {
                    break; // manager dropped — outbound channel closed.
                };
                match req {
                    // The relay session is joined at construction, so
                    // auto-Hello has no work to do here.
                    CollabRequest::Hello { .. } => {}
                    // Both ride the unsigned co-edit path — the host forwards opaque
                    // delta bytes, the receiver applies them with `load_incremental`.
                    CollabRequest::SyncCRDT { data }
                    | CollabRequest::SyncIncremental { data } => {
                        let _ = session.publish_delta(data);
                    }
                    CollabRequest::SignedSyncIncremental { envelope } => {
                        if let Ok(bytes) = postcard::to_stdvec(&*envelope) {
                            let _ = session.publish_signed(bytes);
                        }
                    }
                    CollabRequest::Presence { key, payload } => {
                        // The session stamps the author; we only carry
                        // the (key, payload) body.
                        let _ = session.publish_presence(encode_relay_presence(&key, &payload));
                    }
                    CollabRequest::FetchScrollback { artifact, .. } => {
                        // The host queues deltas per-peer; it doesn't answer seq-range
                        // queries. Reply empty + exhausted so a paging caller unsticks.
                        if inbound_tx
                            .send(CollabEvent::ScrollbackBatch {
                                artifact,
                                ops: Vec::new(),
                                exhausted: true,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            inbound = session.recv() => {
                let Some(inbound) = inbound else {
                    break; // host closed the session.
                };
                if let Some(evt) = translate_relay_inbound(inbound)
                    && inbound_tx.send(evt).is_err()
                {
                    break;
                }
            }
        }
    }
}

/// Encode `(PresenceKey, PresencePayload)` as the opaque body of a
/// relay presence frame; the session adds author attribution on send.
fn encode_relay_presence(key: &PresenceKey, payload: &PresencePayload) -> Vec<u8> {
    postcard::to_stdvec(&(key, payload)).unwrap_or_default()
}

/// Inverse of [`encode_relay_presence`]; `None` for a non-toolkit body
/// so the caller skips it instead of surfacing a bogus event.
fn decode_relay_presence(bytes: &[u8]) -> Option<(PresenceKey, PresencePayload)> {
    postcard::from_bytes(bytes).ok()
}

/// Render a device pubkey as `peer-<hex>` — the same shape the broker
/// mints, keeping downstream attribution on one id format.
fn user_id_from_author(author: [u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(5 + 64);
    s.push_str("peer-");
    for b in author {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Translate a host-side [`qos_relay::Inbound`] into a [`CollabEvent`];
/// `None` for the `Ready` ack or a frame that fails to decode.
fn translate_relay_inbound(inbound: qos_relay::Inbound) -> Option<CollabEvent> {
    match inbound {
        // Unsigned document delta; carries no author, matching the
        // unsigned broker path.
        qos_relay::Inbound::Delta { data, .. } => Some(CollabEvent::CRDTIncremental { data }),
        // Signed document op, attributed to the publishing device key.
        qos_relay::Inbound::SignedDelta { author, op_bytes } => {
            let envelope = postcard::from_bytes::<SignedOp>(&op_bytes).ok()?;
            Some(CollabEvent::SignedCRDTIncremental {
                envelope: Box::new(envelope),
                from_user_id: user_id_from_author(author),
            })
        }
        // The author rode inside the co-edit payload, since the host
        // envelope carries none.
        qos_relay::Inbound::Presence { author, payload } => {
            let (key, payload) = decode_relay_presence(&payload)?;
            let user_id = user_id_from_author(author);
            Some(match payload {
                PresencePayload::Pointer { x, y } => {
                    CollabEvent::RemoteCursorMoved { user_id, x, y }
                }
                other => CollabEvent::RemotePresenceUpdate {
                    user_id,
                    key,
                    payload: other,
                },
            })
        }
        // The subscribe ack has no toolkit-visible equivalent.
        qos_relay::Inbound::Ready { .. } => None,
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
fn translate_presence_event(evt: runtime_collab::PresenceEvent) -> CollabEvent {
    // Cursor keeps the legacy `user_id + x + y` shape; every other
    // variant carries the decoded `PresenceKey`, falling back to a
    // synthetic key if decoding fails.
    match evt.payload {
        runtime_collab::PresencePayload::Cursor { x, y } => CollabEvent::RemoteCursorMoved {
            user_id: evt.user_id,
            x,
            y,
        },
        other => {
            let key: PresenceKey =
                postcard::from_bytes(&evt.key_bytes).unwrap_or_else(|_| PresenceKey::Tab {
                    id: String::from("<undecodable>"),
                });
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
                runtime_collab::PresencePayload::Arrived => PresencePayload::Arrived,
                runtime_collab::PresencePayload::Left => PresencePayload::Left,
                // Keeps the coordinate space its key carries, where
                // `Cursor` has nowhere to put one.
                runtime_collab::PresencePayload::Pointer { x, y } => {
                    PresencePayload::Pointer { x, y }
                }
                // Handled in the outer arm; make match exhaustive.
                runtime_collab::PresencePayload::Cursor { .. } => unreachable!(),
            };
            CollabEvent::RemotePresenceUpdate {
                user_id: evt.user_id,
                key,
                payload,
            }
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
        channel_context, channel_handle, collab_service, context_stream, presence_event,
        presence_handle, presence_payload, presence_stream, scrollback_batch, stream,
    };

    #[derive(Default)]
    struct Captures {
        joined: Vec<(String, Vec<u8>)>,
        signed_ops: Vec<Vec<u8>>,
        unsigned_snapshots: Vec<Vec<u8>>,
        unsigned_incrementals: Vec<Vec<u8>>,
        cursor_publishes: Vec<(f32, f32)>,
        pointer_publishes: Vec<(Vec<u8>, f32, f32)>,
        caret_publishes: Vec<(Vec<u8>, u64)>,
        selection_publishes: Vec<(Vec<u8>, u64, u64)>,
        arrived: Vec<Vec<u8>>,
        left: Vec<Vec<u8>>,
        scrollback_requests: Vec<(u64, u32)>,
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
        Caret { pos: u64 },
        Selection { start: u64, end: u64 },
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
                SeedPayload::Caret { pos } => {
                    payload.init_caret().set_pos(pos);
                }
                SeedPayload::Selection { start, end } => {
                    let mut b = payload.init_selection();
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

        fn subscribe_remote_cursors(
            self: Rc<Self>,
            _params: presence_handle::SubscribeRemoteCursorsParams,
            _results: presence_handle::SubscribeRemoteCursorsResults,
        ) -> Promise<(), capnp::Error> {
            // Not driven — ui::CollaborationManager only opens
            // `subscribeAll`.
            Promise::err(capnp::Error::unimplemented(
                "subscribe_remote_cursors".into(),
            ))
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

        fn send_unsigned_snapshot(
            self: Rc<Self>,
            params: channel_handle::SendUnsignedSnapshotParams,
            _results: channel_handle::SendUnsignedSnapshotResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(d) = params
                .get()
                .and_then(channel_handle::send_unsigned_snapshot_params::Reader::get_data)
            else {
                return Promise::ok(());
            };
            self.captures
                .lock()
                .unwrap()
                .unsigned_snapshots
                .push(d.to_vec());
            Promise::ok(())
        }

        fn send_unsigned_incremental(
            self: Rc<Self>,
            params: channel_handle::SendUnsignedIncrementalParams,
            _results: channel_handle::SendUnsignedIncrementalResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(d) = params
                .get()
                .and_then(channel_handle::send_unsigned_incremental_params::Reader::get_data)
            else {
                return Promise::ok(());
            };
            self.captures
                .lock()
                .unwrap()
                .unsigned_incrementals
                .push(d.to_vec());
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

        fn fetch_scrollback(
            self: Rc<Self>,
            params: channel_handle::FetchScrollbackParams,
            mut results: channel_handle::FetchScrollbackResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(p) = params.get() else {
                return Promise::ok(());
            };
            self.captures
                .lock()
                .unwrap()
                .scrollback_requests
                .push((p.get_before_seq(), p.get_max()));
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
        mgr.send(CollabRequest::SyncCRDT {
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
        mgr.send(CollabRequest::SyncCRDT {
            data: b"nope".to_vec(),
        });
        assert!(mgr.poll().is_empty(), "no events on Disabled backend");
    }

    #[test]
    #[serial_test::serial(qos_user_identity)]
    fn join_channel_uses_empty_identity_when_env_var_unset() {
        // With `QOS_USER_IDENTITY` unset, `load()` returns `ServiceFdMissing`
        // and the manager falls back to the empty-identity path.
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);

        // SAFETY: every test that mutates this variable is serialised on
        // the `qos_user_identity` key, so nothing reads it concurrently.
        unsafe { std::env::remove_var(runtime_identity::IDENTITY_ENV_VAR) };
        let cluster = Uuid::new_v4();
        let _mgr = CollaborationManager::from_sdk_fd_for_test_with_identity(
            fd,
            "alice",
            cluster,
            Vec::new(),
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
        // The channel id carries the whole address, app id included.
        assert_eq!(
            joined[0].0,
            channel_key::format("alice", &cluster.to_string(), ""),
        );
        assert!(
            joined[0].1.is_empty(),
            "identity blob must be empty when env unset"
        );
    }

    #[test]
    fn join_channel_carries_pubkey_bytes_when_identity_loads() {
        // A fresh single-device identity on disk must ride the join_channel
        // call as its pubkey bytes.
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("primary.json");
        let secret = [42u8; 32];
        let sk = ed25519_dalek::SigningKey::from_bytes(&secret);
        let vk = sk.verifying_key();
        let record = serde_json::json!({
            "display_name": "test-user",
            "avatar_color": [1, 2, 3],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": vk.to_bytes(),
                "threshold": 1_u16,
                "total_shares": 1_u16,
                "share_index": 1_u16,
                "secret_bytes": secret.to_vec(),
            },
            "created_at": "2026-07-04T00:00:00Z",
            "schema_version": 1,
        });
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&serde_json::to_vec(&record).unwrap()).unwrap();
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }

        // `new()` also reaches for `QOS_SVC_COLLAB_FD` and would race other
        // tests; use the identity-aware test constructor instead.
        let loaded = runtime_identity::load_from_path(path).expect("load identity");
        let identity_bytes = loaded.group_public_key().to_vec();

        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);
        let cluster = Uuid::new_v4();
        let _mgr = CollaborationManager::from_sdk_fd_for_test_with_identity(
            fd,
            "alice",
            cluster,
            identity_bytes.clone(),
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
        assert_eq!(
            joined[0].1, identity_bytes,
            "identity blob must equal the loaded pubkey bytes"
        );
        assert_eq!(joined[0].1.len(), 32, "Ed25519 pubkey is 32 bytes");
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
        mgr.send(CollabRequest::SignedSyncIncremental {
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

    #[test]
    fn sync_crdt_and_sync_incremental_hit_legacy_sdk_calls() {
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

        mgr.send(CollabRequest::SyncCRDT {
            data: b"snap".to_vec(),
        });
        mgr.send(CollabRequest::SyncIncremental {
            data: b"delta".to_vec(),
        });

        assert!(
            wait_until(
                || {
                    let c = captures.lock().unwrap();
                    !c.unsigned_snapshots.is_empty() && !c.unsigned_incrementals.is_empty()
                },
                Duration::from_secs(2),
            ),
            "both legacy sends must reach the SDK",
        );
        {
            let c = captures.lock().unwrap();
            assert_eq!(c.unsigned_snapshots, vec![b"snap".to_vec()]);
            assert_eq!(c.unsigned_incrementals, vec![b"delta".to_vec()]);
        }
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
                        && c.arrived.len() == 1
                        && c.left.len() == 1
                },
                Duration::from_secs(2),
            ),
            "every presence variant must reach the corresponding SDK method",
        );
        {
            let c = captures.lock().unwrap();
            assert_eq!(
                c.pointer_publishes,
                vec![(expected_key_bytes.clone(), 1.5, 2.5)],
                "a pointer carries the key its publisher chose, like every other \
                 keyed variant here",
            );
            assert!(
                c.cursor_publishes.is_empty(),
                "and does not go out through the keyless call, which would drop \
                 the space on the way",
            );
            assert_eq!(c.caret_publishes, vec![(expected_key_bytes.clone(), 12)]);
            assert_eq!(
                c.selection_publishes,
                vec![(expected_key_bytes.clone(), 3, 9)]
            );
            assert_eq!(c.arrived, vec![expected_key_bytes.clone()]);
            assert_eq!(c.left, vec![expected_key_bytes]);
        }
    }

    #[test]
    fn signed_op_stream_translates_to_signed_crdt_incremental() {
        let mut op = SignedOp::new_unsigned(
            b"remote-payload".to_vec(),
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
                CollabEvent::SignedCRDTIncremental {
                    envelope,
                    from_user_id,
                } => Some((envelope, from_user_id)),
                _ => None,
            })
            .collect();
        assert_eq!(signed.len(), 1, "one SignedCRDTIncremental expected");
        assert_eq!(signed[0].1, "peer-bob");
        assert_eq!(signed[0].0.payload, b"remote-payload");
        assert_eq!(signed[0].0.seq, 42);
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
        let events = drain_events(&mut mgr, Duration::from_secs(2));

        let mut cursor_moves = Vec::new();
        let mut updates = Vec::new();
        for e in events {
            match e {
                CollabEvent::RemoteCursorMoved { x, y, .. } => cursor_moves.push((x, y)),
                CollabEvent::RemotePresenceUpdate { payload, .. } => updates.push(payload),
                _ => {}
            }
        }
        assert!(
            cursor_moves.contains(&(4.0, 5.0)),
            "cursor payload must land on RemoteCursorMoved"
        );
        assert!(
            updates.contains(&PresencePayload::Caret { pos: 7 }),
            "caret variant translated"
        );
        assert!(
            updates.contains(&PresencePayload::Selection { start: 1, end: 3 }),
            "selection variant translated"
        );
        assert!(
            updates.contains(&PresencePayload::Arrived),
            "arrived variant translated"
        );
        assert!(
            updates.contains(&PresencePayload::Left),
            "left variant translated"
        );
    }

    #[test]
    fn fetch_scrollback_returns_batch_event() {
        let mut op = SignedOp::new_unsigned(
            b"old-payload".to_vec(),
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
        assert_eq!(batch.0[0].payload, b"old-payload");
        assert!(batch.1, "seeded broker reports exhausted");
        assert_eq!(
            captures.lock().unwrap().scrollback_requests,
            vec![(10, 50)],
            "before_seq / max round-tripped verbatim",
        );
    }

    #[test]
    fn translate_presence_event_is_symmetric_over_all_payload_variants() {
        // Straight unit test of `translate_presence_event` — no broker or
        // threading needed. Covers every payload variant + the undecodable-key fallback.
        let key = PresenceKey::Widget {
            path: vec![crate::presence::PathSegment::Field("body".into())],
        };
        let key_bytes = postcard::to_stdvec(&key).unwrap();

        let e = translate_presence_event(runtime_collab::PresenceEvent {
            user_id: "peer".into(),
            key_bytes: key_bytes.clone(),
            payload: SdkPayload::Cursor { x: 7.0, y: 8.0 },
        });
        matches!(e, CollabEvent::RemoteCursorMoved { .. });

        let e = translate_presence_event(runtime_collab::PresenceEvent {
            user_id: "peer".into(),
            key_bytes,
            payload: SdkPayload::Caret { pos: 3 },
        });
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
        if let CollabEvent::RemotePresenceUpdate { key: k, .. } = e {
            assert!(matches!(k, PresenceKey::Tab { .. }));
        } else {
            panic!("expected RemotePresenceUpdate on undecodable key");
        }
    }
}
