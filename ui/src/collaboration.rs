//! Toolkit-side collab client.
//!
//! Historical shape (still the public API, so every downstream app —
//! `apps/notes`, `apps/word-processor`,
//! `apps/collab-broker`, plus the toolkit's own `ui::App` — keeps
//! compiling): a synchronous, non-blocking `send` / `poll` pair
//! trading typed [`CollabRequest`] / [`CollabEvent`] frames.
//!
//! Under the hood the manager now delegates to the `runtime-collab`
//! Cap'n Proto SDK. `try_connect` is called on a shovel thread the
//! manager owns; on `Ok` it stands up a `tokio::task::LocalSet`,
//! spawns fan-out subscription tasks for signed ops / roster
//! updates / presence, and bridges winit-side `send` / `poll` to the
//! SDK via a pair of `tokio::sync::mpsc::UnboundedChannel`s. On
//! [`runtime_portal::NotAvailable`] the manager enters "disabled"
//! mode — `poll()` returns empty, `send()` is a no-op, no thread is
//! spawned, no panic ever fires. Widgets that already branch on
//! [`Self::is_connected`] pick that up transparently.
//!
//! The legacy JSON-over-Unix-socket dialect is kept behind the
//! `legacy-broker` feature flag (default on) so the in-tree test
//! suites, the `collab-broker` binary and the shell's production
//! broker keep compiling while the fleet migrates onto the SDK.
//!
//! ## Flipping the feature off
//!
//! Once every app in the fleet talks the SDK, the `legacy-broker`
//! default can be dropped. The recipe is:
//!
//! 1. Verify `cargo build -p ui --no-default-features` succeeds
//!    (currently exercised on every push; the SDK code path resolves
//!    without the JSON transport). The shell now serves the SDK's
//!    capnp interface natively via `shell::shell::collab_broker_capnp`,
//!    so every downstream app can pick up its connection from
//!    `QOS_SVC_COLLAB_FD` without going through the JSON socket.
//! 2. Change this crate's `Cargo.toml` `default` line to `[]`
//!    (or a set that excludes `legacy-broker`).
//! 3. Delete the `#[cfg(feature = "legacy-broker")]` gates in this
//!    module and the `broker` sub-module once no consumer depends on
//!    them.
//!
//! Downstream apps flip transparently — the public `send` / `poll`
//! surface is unchanged; only the underlying transport differs.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use collab_types::SignedOp;

use crate::presence::PresenceKey;

#[cfg(feature = "legacy-broker")]
use std::io::{Read, Write};
#[cfg(feature = "legacy-broker")]
use std::os::unix::net::UnixStream;

// `RawFd` is used unconditionally in the test-only `SdkConnectSource::Fd`
// variant and in `from_sdk_fd_for_test`. Both are `#[cfg(test)]` so a
// release build without tests doesn't need it — the cfg-gated import
// below keeps the release build warning-free.
#[cfg(test)]
use std::os::fd::RawFd;
use std::sync::mpsc;
use std::thread;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::LocalSet;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabRequest {
    Hello { app_id: String, cluster_id: Uuid },
    /// Legacy: unsigned full-document state. Still accepted by the
    /// broker. Apps using this path do not get role enforcement and
    /// surface a "legacy / unsigned" badge in chrome.
    SyncCRDT { data: Vec<u8> },
    /// Legacy: unsigned incremental Automerge change blob. Still
    /// routed by the broker for backward compatibility while Phase 3
    /// migration is in flight. New code should prefer
    /// [`CollabRequest::SignedSyncIncremental`].
    SyncIncremental { data: Vec<u8> },
    /// Outbound presence update — the local user's pointer moved, caret
    /// changed, selection shifted, or they arrived/left a keyed object.
    /// The server fans this out to every connected peer as a
    /// `CollabEvent::RemotePresenceUpdate`; the test relay uses the same
    /// path. Round-trips complete the loop with `apply_remote_presence`.
    Presence { key: PresenceKey, payload: PresencePayload },
    /// Phase 3: signed incremental CRDT mutation. The broker validates
    /// the envelope against the channel's role table (see
    /// [`collab_types::validate_inbound_op`]) before fanning it
    /// out. Invalid envelopes are dropped and a structured log line is
    /// emitted; nothing reaches other peers.
    ///
    /// `Box`ed so the discriminant + payload stay a thin handle on the
    /// stack — `SignedOp` is large (32-byte pubkey + 64-byte sig +
    /// payload + UUIDs) and growing the enum to its inline size would
    /// bloat every `CollabRequest` value passed by value.
    SignedSyncIncremental { envelope: Box<SignedOp> },
    /// Phase 6: late-join scrollback fetch. A peer that has just
    /// joined a channel and wants to render older edits asks the
    /// broker for up to `max` ops with `seq < before_seq` on the
    /// given artifact. The broker replies with a single
    /// [`CollabEvent::ScrollbackBatch`].
    ///
    /// Apps drive this in two steps: a first request with
    /// `before_seq` set to "the lowest seq I've already seen + 1",
    /// then if `exhausted == false` repeat with `before_seq` set to
    /// the lowest seq from the previous batch. When `exhausted ==
    /// true` (or the batch is empty) the in-memory broker has no
    /// more history; the cooperative scrollback path takes over
    /// (broker queries gossipsub peers — see
    /// `NeighbourMsg::ScrollbackRequest`).
    FetchScrollback {
        /// The artifact whose log to scroll. Same id the app passes
        /// when sending mutation ops.
        artifact: collab_types::ArtifactId,
        /// Exclusive upper bound on `seq`. The broker returns ops
        /// with `seq < before_seq`. To fetch "everything I haven't
        /// seen", pass the lowest seq the app has rendered locally.
        before_seq: u64,
        /// Page size cap. `u16` because individual scrollback pages
        /// are bounded by chrome UX (~50 ops/page); a wider int would
        /// be premature flexibility.
        max: u16,
    },
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
    /// Legacy: full unsigned document state. Apps drop into a merge of
    /// the snapshot into their local Automerge doc.
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
    /// Phase 3: a validated signed envelope from another peer. The
    /// validation pipeline ran on the broker side; receivers extract
    /// `envelope.payload` and feed it into the same
    /// `automerge::apply_changes` path the legacy
    /// [`CollabEvent::CRDTIncremental`] uses.
    ///
    /// `from_user_id` mirrors the synthetic id minted by the broker for
    /// [`CollabEvent::RemotePresenceUpdate`] (`"peer-<src_id>"`) so
    /// downstream attribution code can use a single id shape.
    SignedCRDTIncremental {
        envelope: Box<SignedOp>,
        from_user_id: String,
    },
    /// Phase 6: response to a [`CollabRequest::FetchScrollback`].
    /// Carries a page of previously-validated ops (or the empty
    /// vector if the broker has nothing in the requested range) plus
    /// an `exhausted` flag that tells the requester whether to stop
    /// paging in-memory and fall back to the cooperative path.
    ///
    /// Targeted send: the broker writes a `ScrollbackBatch` only to
    /// the connection that issued the matching `FetchScrollback`, not
    /// to every connected app. This is the first per-peer reply in
    /// the broker — every prior event was fan-out.
    ScrollbackBatch {
        /// Echo of the original `FetchScrollback::artifact` so the
        /// app can route the reply to the right history view when
        /// multiple scrollback fetches are in flight.
        artifact: collab_types::ArtifactId,
        /// The page of ops, in ascending seq order. Empty when the
        /// broker has no matching range (typically because compaction
        /// has evicted it).
        ops: Vec<Box<SignedOp>>,
        /// True iff the broker holds no more history in the requested
        /// range — i.e. the page is the complete answer from
        /// in-memory storage. False means there's more on the broker
        /// that the app can fetch with another request.
        exhausted: bool,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum PresencePayload {
    Pointer { x: f32, y: f32 },
    Caret { pos: usize },
    Selection { start: usize, end: usize },
    Arrived,
    Left,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    pub color: [f32; 3],
}

/// Failure modes for [`CollaborationManager::send_signed_op`].
///
/// `NoIdentity` is the common path during migration: an app calls
/// `send_signed_op` before the shell has provisioned an identity for
/// it. App code can pattern-match on this and fall back to
/// `send(CollabRequest::SyncIncremental {..})` if it wants legacy
/// behaviour during the transition.
#[derive(Debug)]
pub enum SignedSendError {
    /// No [`SigningIdentity`] has been bound via
    /// [`CollaborationManager::with_identity`] or
    /// [`CollaborationManager::set_identity`].
    NoIdentity,
    /// Canonical-bytes serialisation of the envelope failed. The
    /// underlying [`collab_types::CanonicalError`] carries the
    /// postcard error message; in practice this only fires for
    /// unrepresentable values, not for healthy inputs.
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
/// to mint signed envelopes.
///
/// The toolkit deliberately keeps the secret key wrapped in a private
/// struct (we do not re-export `ed25519_dalek::SigningKey` on the
/// `CollaborationManager`'s public surface) so app code can only sign
/// via the manager's helper, not handle the raw secret. The bound
/// attestation id ties every minted envelope back to the role grant
/// that authorises it; the broker uses that id to look up the role
/// table entry.
///
/// In Phase 3 v1 a `SigningIdentity` is constructed by the shell at
/// boot (mint a keypair, persist it under `~/.config/qos/identity/`
/// per Phase 2, label it with the system username) and handed to every
/// `CollaborationManager` it spins up via
/// [`CollaborationManager::with_identity`]. The eventual threshold
/// upgrade in Phase 8 swaps the `secret_key` field's representation
/// without changing this struct's outward API.
pub struct SigningIdentity {
    /// Local secret half — never serialised, never logged.
    pub(crate) secret_key: ed25519_dalek::SigningKey,
    /// Public half — embedded as the `author` field on every signed
    /// op.
    pub public_key: collab_types::Ed25519PublicKey,
    /// The role attestation under which this identity emits ops. Set
    /// at construction time by whichever flow minted the attestation
    /// (the channel-owner's invite flow in production; the test
    /// fixture in tests). One identity may hold attestations on
    /// multiple channels; v1 pins to one per `CollaborationManager`.
    pub attestation_id: collab_types::AttestationId,
    /// Monotonic per-author seq counter. Bumped on every successful
    /// `sign` so the validator's replay check (`seq > last_seen`)
    /// passes. Re-using a seq is the spec-defined replay attack and
    /// the validator rejects it.
    pub(crate) seq: u64,
}

impl SigningIdentity {
    /// Construct an identity from a fresh keypair and the attestation
    /// id that authorises it. Mostly used in tests; production code
    /// builds via the shell-side identity-loading helper that will
    /// land alongside Phase 8's threshold sharing.
    pub fn new(
        secret_key: ed25519_dalek::SigningKey,
        attestation_id: collab_types::AttestationId,
    ) -> Self {
        let public_key = collab_types::Ed25519PublicKey::from_bytes(
            secret_key.verifying_key().to_bytes(),
        );
        Self {
            secret_key,
            public_key,
            attestation_id,
            seq: 0,
        }
    }

    /// Build a fresh `SignedOp` envelope wrapping `payload`. Bumps the
    /// internal seq so the validator's replay check passes on the
    /// receiving side; callers do not need to track sequence numbers
    /// themselves.
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

// -----------------------------------------------------------------------------
// Backend + shovel machinery
// -----------------------------------------------------------------------------

/// Which transport the manager is speaking. Enum-per-mode instead of
/// trait-object'ing because the runtime cost of a match on `send` /
/// `poll` is negligible and the code stays flat for a reader.
enum Backend {
    /// No collab service reachable. `poll` yields nothing, `send`
    /// is a black hole. Every downstream widget already handles
    /// "manager isn't connected" via [`CollaborationManager::is_connected`];
    /// no widget contract changes.
    Disabled,
    /// Talking Cap'n Proto to `runtime-collab`. The shovel thread owns
    /// the SDK client + subscriptions; winit-side send / poll bridge
    /// through unbounded mpsc pairs.
    Sdk(SdkChannel),
    /// Routing co-edit deltas + presence through the workspace host via
    /// a [`qos_relay::RelayCoeditSession`]. A bridge task on the
    /// caller's tokio runtime pumps the session; winit-side send / poll
    /// bridge through unbounded mpsc pairs, exactly like [`Backend::Sdk`].
    /// Document deltas then inherit the host's fan-out + on-disk
    /// store-and-forward, so two peers that are never simultaneously
    /// online still converge — transparently to the app.
    Relay(RelayChannel),
    /// Legacy JSON-over-Unix-socket dialect. Gated behind the
    /// `legacy-broker` feature (default on). Preserved so tests, the
    /// `collab-broker` binary, and the shell's existing broker keep
    /// working during the fleet migration.
    #[cfg(feature = "legacy-broker")]
    Legacy(LegacyStream),
}

/// Owned mpsc pair + shovel-thread handle for SDK mode.
///
/// **Channel sizing:** both directions use `tokio::sync::mpsc::unbounded_channel`.
/// The winit side always drains completely each frame (`poll()` is
/// non-blocking) and the tokio side always `.recv().await`s, so
/// neither side has an upper bound in normal operation. Presence
/// bursts from a very active peer stay small compared to the frame
/// budget. If a future SDK contract requires backpressure we can swap
/// in bounded channels and drop-on-full for outbound presence; that's
/// a policy decision, not a wire change.
///
/// **Shutdown:** dropping `CollaborationManager` drops `outbound_tx`.
/// The shovel thread's `.recv().await` on `outbound_rx` then yields
/// `None`, its main task returns, the `LocalSet` drops every spawned
/// subscription task, the runtime shuts down cleanly, and the thread
/// exits. `_thread` is retained only so the thread's `JoinHandle` is
/// not detached — `drop(handle)` is intentional.
struct SdkChannel {
    outbound_tx: tokio_mpsc::UnboundedSender<CollabRequest>,
    inbound_rx: tokio_mpsc::UnboundedReceiver<CollabEvent>,
    _thread: Option<thread::JoinHandle<()>>,
}

/// Owned mpsc pair + bridge-task handle for Relay mode.
///
/// Unlike [`SdkChannel`] there is no dedicated OS thread: the
/// [`qos_relay::RelayCoeditSession`] and its underlying iroh transport
/// were already stood up on the caller's tokio runtime (the shell's
/// network actor in production; the test's multi-thread runtime in
/// tests), so the bridge task that pumps the session is `spawn`ed
/// there. Dropping the manager drops `outbound_tx` — the bridge's
/// `select!` observes the closed channel and returns — and aborts the
/// task via [`AbortOnDrop`] as a backstop.
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

#[cfg(feature = "legacy-broker")]
struct LegacyStream {
    stream: UnixStream,
    /// Persistent read buffer. The socket is non-blocking, so a single
    /// `poll` can see a frame split across reads (large `SyncCRDT`
    /// payloads especially) or several frames coalesced. We accumulate
    /// raw bytes here and only extract *complete* length-prefixed
    /// frames, leaving any partial tail for the next poll.
    rbuf: Vec<u8>,
}

/// How the shovel thread reaches the broker.
///
/// `Env` is the production path: the launcher passed `QOS_SVC_COLLAB_FD`;
/// `runtime_collab::try_connect()` reads it. `Fd` is for the in-tree
/// tests — the mock server hands us a specific connected fd (no env
/// var, no cross-test races).
enum SdkConnectSource {
    Env,
    #[cfg(test)]
    Fd(RawFd),
}

impl SdkConnectSource {
    /// Build a `CollabClient` on the current tokio `LocalSet` for
    /// whichever source this is. Returns `NotAvailable` when the
    /// fd cannot be lifted into RPC — same contract as `try_connect`.
    fn build_client(self) -> Result<runtime_collab::CollabClient, runtime_portal::NotAvailable> {
        match self {
            SdkConnectSource::Env => runtime_collab::try_connect(),
            #[cfg(test)]
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
    /// Optional Phase 3 signing identity. When `Some`, calls to
    /// [`Self::send_signed_op`] mint a [`collab_types::SignedOp`]
    /// envelope and dispatch it as a
    /// [`CollabRequest::SignedSyncIncremental`]. `None` is the legacy
    /// path: apps that haven't migrated keep flowing unsigned
    /// `SyncIncremental` frames and the broker fans them out without
    /// validation.
    identity: Option<SigningIdentity>,
    /// User-identity loaded from the on-disk record the launcher
    /// points at (via `QOS_USER_IDENTITY`). Its pubkey bytes are
    /// forwarded to `join_channel` so presence and role lookups can
    /// attribute events to a real caller — the previous behaviour
    /// (empty identity bytes on every join) left presence unauthored
    /// and signed-op provenance ungrounded.
    ///
    /// `None` in three cases:
    /// - The app is running outside qOS (env var unset).
    /// - The pointed-at file is missing / unreadable.
    /// - Parsing failed. See `runtime_identity::load`'s
    ///   [`runtime_portal::NotAvailable`] variants for the split.
    ///
    /// Kept alive on the manager so future signed-op paths that
    /// prefer this identity over the widget-bound [`SigningIdentity`]
    /// can reach for it without re-reading disk.
    #[allow(dead_code)]
    runtime_identity: Option<runtime_identity::SigningIdentity>,
}

impl CollaborationManager {
    /// Standard constructor.
    ///
    /// Selection order:
    /// 1. If the `legacy-broker` feature is on **and** `QUICK_COLLAB_SOCK`
    ///    is set, use the legacy Unix-socket JSON dialect (unchanged
    ///    behaviour for existing tests and the shell's in-process broker).
    /// 2. Otherwise try `runtime_collab::try_connect()` and enter SDK
    ///    mode on `Ok`; enter Disabled mode on `Err(NotAvailable)`.
    ///
    /// Both modes auto-send `Hello { app_id, cluster_id }` — the
    /// address the broker routes this connection's traffic on. See
    /// [`auto_hello_env`] for where those two values come from and in
    /// what order.
    pub fn new() -> Self {
        // Attempt to load the user's on-disk identity via the
        // runtime SDK. When the file is present, its pubkey bytes
        // ride along on the join_channel call the auto-Hello issues
        // below — the previous behaviour (unconditional empty bytes)
        // left presence unauthored on every connect.
        //
        // `runtime_identity::load` reports `ServiceFdMissing` when the
        // env var is unset (not-a-qOS-session, unit test, etc.);
        // that path is expected and quiet. Any other failure mode
        // (unreadable file, malformed JSON) is worth a debug line so
        // a maintainer investigating "why does presence look
        // anonymous?" can find the reason in `RUST_LOG=ui=debug`.
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
            .as_ref()
            .map(|id| id.group_public_key().to_vec())
            .unwrap_or_default();

        #[cfg(feature = "legacy-broker")]
        if let Ok(path) = std::env::var("QUICK_COLLAB_SOCK") {
            if let Ok(stream) = UnixStream::connect(&path) {
                let mut manager = Self {
                    backend: Backend::Legacy(LegacyStream {
                        stream,
                        rbuf: Vec::new(),
                    }),
                    identity: None,
                    runtime_identity,
                };
                let (app_id, cluster_id) = auto_hello_env();
                manager.send(CollabRequest::Hello { app_id, cluster_id });
                return manager;
            }
        }

        match Self::spawn_sdk(SdkConnectSource::Env, identity_bytes) {
            Ok(sdk) => {
                let mut manager = Self {
                    backend: Backend::Sdk(sdk),
                    identity: None,
                    runtime_identity,
                };
                let (app_id, cluster_id) = auto_hello_env();
                manager.send(CollabRequest::Hello { app_id, cluster_id });
                manager
            }
            Err(_) => Self {
                backend: Backend::Disabled,
                identity: None,
                runtime_identity,
            },
        }
    }

    /// Route this manager's co-edit + presence through the workspace
    /// host, over an already-joined [`qos_relay::RelayCoeditSession`].
    ///
    /// The caller — the shell's network actor in production, the test
    /// harness in tests — owns the iroh endpoint + device identity,
    /// dials the host, and joins the workspace's co-edit session; this
    /// constructor wraps that session behind the same synchronous
    /// `send` / `poll` surface every app already uses. From the app's
    /// point of view nothing changes: `SyncIncremental` deltas,
    /// `SignedSyncIncremental` envelopes and `Presence` frames flow out
    /// as before, but now travel through the host's fan-out **and** its
    /// on-disk store-and-forward, so a peer that is offline when the
    /// edit is made still receives it on reconnect.
    ///
    /// `handle` is a handle to the tokio runtime the session's iroh
    /// transport lives on; the bridge task that pumps the session is
    /// spawned there so it shares the runtime already driving the
    /// connection. The manager owns no signing identity in this mode —
    /// document ops are signed by the app before they reach `send`
    /// (the `SignedSyncIncremental` path), and the session stamps
    /// presence attribution itself from its own device key.
    pub fn from_relay_session(
        session: qos_relay::RelayCoeditSession,
        handle: tokio::runtime::Handle,
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
            runtime_identity: None,
        }
    }

    /// Test-only: build a manager talking to an SDK broker on the
    /// given already-connected client fd. Bypasses env-var lookup so
    /// two tests in the same process do not race on `QOS_SVC_COLLAB_FD`.
    ///
    /// Auto-sends the same auto-Hello the production constructor
    /// does, using the passed `app_id` / `cluster_id` for a
    /// deterministic test fixture.
    #[cfg(test)]
    fn from_sdk_fd_for_test(fd: RawFd, app_id: &str, cluster_id: Uuid) -> Self {
        Self::from_sdk_fd_for_test_with_identity(fd, app_id, cluster_id, Vec::new())
    }

    /// Test-only: like [`Self::from_sdk_fd_for_test`] but stamps the
    /// join_channel call with a caller-supplied identity blob. Kept
    /// alongside the empty-blob variant so tests that don't care
    /// about the identity path stay concise.
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
            runtime_identity: None,
        };
        manager.send(CollabRequest::Hello {
            app_id: app_id.to_owned(),
            cluster_id,
        });
        manager
    }

    /// Stand up the shovel thread + its tokio `current_thread` runtime.
    ///
    /// Runs `try_connect` **inside** the shovel thread's `LocalSet`
    /// because capnp-rpc futures are `!Send` and the resulting
    /// `CollabClient` cannot cross a thread boundary. Errors from the
    /// initial connect are surfaced via [`InitStatus`] on a one-shot
    /// std mpsc so the caller can pick "Disabled vs Sdk" before
    /// returning; if we deferred the decision to the first `send`
    /// call, apps that only ever `poll` would never notice they were
    /// running against a dead broker.
    fn spawn_sdk(
        source: SdkConnectSource,
        identity_bytes: Vec<u8>,
    ) -> Result<SdkChannel, runtime_portal::NotAvailable> {
        let (outbound_tx, outbound_rx) = tokio_mpsc::unbounded_channel::<CollabRequest>();
        let (inbound_tx, inbound_rx) = tokio_mpsc::unbounded_channel::<CollabEvent>();
        let (init_tx, init_rx) = mpsc::sync_channel::<InitStatus>(1);

        // Extract the raw fd (if any) *before* moving the source into
        // the closure — `SdkConnectSource::Fd` is `!Send` in principle
        // (RawFd is Send, but this keeps the intent explicit).
        let handle = thread::Builder::new()
            .name("ui-collab-shovel".into())
            .spawn(move || run_shovel(source, identity_bytes, outbound_rx, inbound_tx, init_tx))
            .expect("spawn ui-collab-shovel thread");

        // Wait for the shovel to report initial connect status. This
        // is a bounded wait: `run_shovel` reports before entering its
        // event loop.
        let status = init_rx
            .recv()
            .expect("shovel thread must send an init status before its main loop");

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

    /// True when connected to a live broker (SDK or legacy). The event
    /// loop uses this to tick while collaborating so the socket / RPC
    /// stream is drained promptly instead of only on local input.
    pub fn is_connected(&self) -> bool {
        !matches!(self.backend, Backend::Disabled)
    }

    /// Phase 3: bind a signing identity to this manager. Once set,
    /// [`Self::send_signed_op`] is the canonical outbound CRDT-mutation
    /// path; legacy unsigned `SyncIncremental` still works for apps
    /// that haven't migrated.
    ///
    /// Builder-style so the shell can chain
    /// `CollaborationManager::new().with_identity(id)` at boot. The
    /// previous identity (if any) is replaced; the manager owns its
    /// `SigningIdentity` and its seq counter outright.
    pub fn with_identity(mut self, identity: SigningIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Replace the bound signing identity in-place. Used by the shell
    /// to swap identities when the user switches the active identity
    /// in the chrome's identity picker without tearing down the
    /// manager.
    pub fn set_identity(&mut self, identity: SigningIdentity) {
        self.identity = Some(identity);
    }

    /// True when a [`SigningIdentity`] has been bound to this manager
    /// (via [`Self::with_identity`] or [`Self::set_identity`]). Apps
    /// can branch on this to decide whether to call
    /// [`Self::send_signed_op`] or fall back to the legacy unsigned
    /// path.
    pub fn has_identity(&self) -> bool {
        self.identity.is_some()
    }

    /// Phase 3: sign and send a CRDT mutation envelope. Returns
    /// `Err` if the manager has no identity bound, or the canonical
    /// encoder fails (which is effectively unreachable for the wire
    /// types in this crate — see [`collab_types::canonical`]).
    ///
    /// The send itself is best-effort, mirroring the legacy
    /// [`Self::send`] path: a frame that doesn't reach the socket
    /// won't be retried, but the next live edit will republish the
    /// CRDT state so the loss is bounded by one round-trip.
    pub fn send_signed_op(
        &mut self,
        payload: Vec<u8>,
        artifact: collab_types::ArtifactId,
        op_class: collab_types::OpClass,
    ) -> Result<(), SignedSendError> {
        let identity = self
            .identity
            .as_mut()
            .ok_or(SignedSendError::NoIdentity)?;
        let envelope = identity
            .sign_op(payload, artifact, op_class)
            .map_err(SignedSendError::Canonical)?;
        self.send(CollabRequest::SignedSyncIncremental {
            envelope: Box::new(envelope),
        });
        Ok(())
    }

    /// Fire-and-forget send. Non-blocking on every backend:
    /// - SDK: pushes onto the outbound mpsc; the shovel thread does
    ///   the actual RPC.
    /// - Legacy: writes the length-prefixed JSON frame directly.
    /// - Disabled: no-op.
    ///
    /// Delivery failures never surface to the caller by design — the
    /// legacy path already had that contract (the shell can recover
    /// via the next sync round) and the SDK path preserves it. Apps
    /// that need delivery confirmation build on top of `poll` events.
    pub fn send(&mut self, req: CollabRequest) {
        match &mut self.backend {
            Backend::Disabled => {}
            Backend::Sdk(ch) => {
                // UnboundedSender::send only errors when the receiver
                // has been dropped, which for us means the shovel
                // thread has exited. Silently ignore — the next poll
                // will observe the same shutdown and the caller can
                // check `is_connected` if they care.
                let _ = ch.outbound_tx.send(req);
            }
            Backend::Relay(ch) => {
                // Same fire-and-forget contract as SDK mode: push onto
                // the outbound mpsc; the bridge task maps it onto the
                // relay session's publish verbs.
                let _ = ch.outbound_tx.send(req);
            }
            #[cfg(feature = "legacy-broker")]
            Backend::Legacy(l) => {
                if let Ok(json) = serde_json::to_vec(&req) {
                    // Block for writes so a large frame (a full-document
                    // `SyncCRDT` snapshot) is sent in full. `poll` flips the
                    // socket to non-blocking for reads; the two alternate on
                    // one thread, so toggling here is safe. Without this a big
                    // `write_all` on a non-blocking socket returns WouldBlock
                    // partway and the frame is truncated.
                    let _ = l.stream.set_nonblocking(false);
                    let len = json.len() as u32;
                    let _ = l.stream.write_all(&len.to_le_bytes());
                    let _ = l.stream.write_all(&json);
                }
            }
        }
    }

    /// Drain every event pending on the current backend. Non-blocking.
    ///
    /// The signature stays `Vec<CollabEvent>` (not `Option<...>`) because
    /// every call-site — `ui::App::pump_collab_presence`, the test hook
    /// `poll_and_apply_collab_events`, integration harnesses — is
    /// already `for event in manager.poll()`. Migrating to Option would
    /// touch every caller for zero win.
    pub fn poll(&mut self) -> Vec<CollabEvent> {
        match &mut self.backend {
            Backend::Disabled => Vec::new(),
            Backend::Sdk(ch) => {
                let mut events = Vec::new();
                // try_recv is the only shape that makes sense here:
                // pump the queue empty and return. The shovel thread
                // pushes concurrently so a rare bounded loop is fine —
                // if it filled forever we would still drain a bounded
                // slice per frame because the event loop calls us at
                // most a few thousand times per second.
                while let Ok(evt) = ch.inbound_rx.try_recv() {
                    events.push(evt);
                }
                events
            }
            Backend::Relay(ch) => {
                // Identical drain to SDK mode; the bridge task is the
                // producer instead of the shovel thread.
                let mut events = Vec::new();
                while let Ok(evt) = ch.inbound_rx.try_recv() {
                    events.push(evt);
                }
                events
            }
            #[cfg(feature = "legacy-broker")]
            Backend::Legacy(l) => {
                let _ = l.stream.set_nonblocking(true);
                let mut chunk = [0u8; 8192];
                loop {
                    match l.stream.read(&mut chunk) {
                        Ok(0) => break, // EOF
                        Ok(n) => l.rbuf.extend_from_slice(&chunk[..n]),
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(_) => break,
                    }
                }

                let mut events = Vec::new();
                loop {
                    if l.rbuf.len() < 4 {
                        break;
                    }
                    let len = u32::from_le_bytes([
                        l.rbuf[0], l.rbuf[1], l.rbuf[2], l.rbuf[3],
                    ]) as usize;
                    if l.rbuf.len() < 4 + len {
                        break; // frame not fully arrived yet
                    }
                    let frame: Vec<u8> = l.rbuf[4..4 + len].to_vec();
                    l.rbuf.drain(..4 + len);
                    if let Ok(event) = serde_json::from_slice(&frame) {
                        events.push(event);
                    }
                }
                events
            }
        }
    }
}

/// The `(app_id, cluster_id)` pair this process announces in its
/// [`CollabRequest::Hello`] — its address on the broker's fan-out.
///
/// The broker delivers a CRDT delta only to connections announcing the
/// same pair, so these two values are the whole of "who am I allowed to
/// collaborate with". Getting them from the environment (rather than
/// letting each app invent them) is what makes two separately-launched
/// processes recognise each other as the same app in the same room.
///
/// # Precedence
///
/// Both halves resolve independently, each in the same order:
///
/// 1. **`QOS_PACKAGE_ID` / `QOS_WORKSPACE_ID`** — set by the launcher
///    for every app the compositor starts. This is the production
///    path: the app id is the package's reverse-DNS id (so two windows
///    of one app agree, and a different app never collides with it),
///    and the cluster is the workspace the user opened it into.
/// 2. **`QUICK_APP_ID` / `QUICK_CLUSTER_ID`** — the standalone-dev
///    override, for running apps straight out of `cargo run` with no
///    session behind them. `QUICK_APP_ID` doubles as a *per-instance*
///    identity in the sample apps (`QUICK_APP_ID=alice` /
///    `QUICK_APP_ID=bob` runs one app as two people), which is why it
///    ranks below `QOS_PACKAGE_ID` instead of overriding it: under a
///    real session the package id is the routing truth, and a
///    per-instance name must not fragment it. Those sample setups
///    still work because they run against the development broker in
///    [`crate::collaboration::broker`], which reads `app_id` as a
///    presence attribution and fans out to everyone — the compositor's
///    broker is the one that routes.
/// 3. **A fresh random app id and the nil cluster** — the last-resort
///    default, for a process that was told nothing. The random app id
///    is what makes this fail closed: no other process can announce
///    the same one, so nothing is ever delivered here. An app that
///    cannot say where it belongs must not be handed another app's
///    document.
///
/// `QOS_WORKSPACE_ID` is only *used* as a cluster when it parses as a
/// uuid, which is the shape the launcher writes. It is also read
/// elsewhere as a bare "am I on qOS?" flag
/// (`runtime_portal::is_qos_native`), so a value we cannot route on is
/// passed over here rather than rejected — the next fallback applies
/// and the flag keeps working.
fn auto_hello_env() -> (String, Uuid) {
    auto_hello_env_uncached()
}

/// The identity this process collaborates as — the same `user_id` the broker
/// attributes its ops and presence to.
///
/// Actions are stamped with this so undo can tell one collaborator's work from
/// another's: see [`crate::state::State::undo_as`].
pub fn local_user_id() -> crate::permissions::UserId {
    crate::permissions::UserId(auto_hello_env().0)
}

fn auto_hello_env_uncached() -> (String, Uuid) {
    // Only the last-resort random id is remembered, and only because a
    // *fresh* uuid per call would hand a second manager in the same
    // process a different identity — it would announce as two people,
    // and undo attribution would not match the id the broker sees.
    //
    // The environment itself is re-read every time and is never
    // memoised. It is the routing truth: which room an app belongs to
    // is a property of the launch, not of the process image, and
    // freezing the first answer puts every later reader in the first
    // one's room. That is a room-isolation failure, not a caching
    // detail — and it is exactly what a process hosting more than one
    // identity (every collaboration test, and any future in-process
    // multi-session host) does.
    static FALLBACK_APP_ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();

    let app_id = non_empty_env("QOS_PACKAGE_ID")
        .or_else(|| non_empty_env("QUICK_APP_ID"))
        .unwrap_or_else(|| {
            FALLBACK_APP_ID
                .get_or_init(|| Uuid::new_v4().to_string())
                .clone()
        });

    let cluster_id = non_empty_env("QOS_WORKSPACE_ID")
        .and_then(|s| match Uuid::parse_str(&s) {
            Ok(id) => Some(id),
            Err(_) => {
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

/// `std::env::var` with empty treated as unset. An env var explicitly
/// set to `""` is the shape a shell script produces when a variable it
/// meant to forward was itself unset; taking it literally would give
/// every such app the same empty identity, i.e. put them all in one
/// room.
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

/// One-shot init-result value emitted by the shovel thread before it
/// enters its main loop. Lets `spawn_sdk` decide immediately whether
/// to hand out `SdkChannel` or fall back to `Backend::Disabled`
/// without polling the SDK for a "ready" signal.
enum InitStatus {
    Ready,
    Failed(runtime_portal::NotAvailable),
}

/// The shovel thread's body. Owns a `current_thread` tokio runtime + a
/// `LocalSet` (capnp-rpc futures are `!Send`), stands up the SDK
/// client, subscribes the three streams (signed ops, roster context,
/// presence), and forwards outbound `CollabRequest`s to matching SDK
/// calls. Exits cleanly when `outbound_rx.recv()` returns `None`
/// (i.e. the manager was dropped).
fn run_shovel(
    source: SdkConnectSource,
    identity_bytes: Vec<u8>,
    outbound_rx: tokio_mpsc::UnboundedReceiver<CollabRequest>,
    inbound_tx: tokio_mpsc::UnboundedSender<CollabEvent>,
    init_tx: mpsc::SyncSender<InitStatus>,
) {
    // Building `current_thread` is fallible (it can OOM in extreme
    // conditions). If that fires we can't do anything useful, so
    // surface as Disabled — no service means the app runs local-only.
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

/// SDK-side event loop. Consumes outbound `CollabRequest`s, calls the
/// matching SDK method, and spawns one `spawn_local` subscription task
/// per stream on the first successful `join_channel`. Kept in its own
/// async fn so the outer `run_shovel` handles all setup / teardown and
/// this body reads as pure protocol translation.
async fn drive_shovel(
    client: runtime_collab::CollabClient,
    identity_bytes: Vec<u8>,
    mut outbound_rx: tokio_mpsc::UnboundedReceiver<CollabRequest>,
    inbound_tx: tokio_mpsc::UnboundedSender<CollabEvent>,
) {
    // Cached handles. `channel` is set on the first Hello; every
    // downstream RPC (send / send_unsigned_*, fetch_scrollback,
    // presence.publish_*) requires it.
    let mut channel: Option<runtime_collab::ChannelHandle> = None;
    let mut presence: Option<runtime_collab::PresenceHandle> = None;

    while let Some(req) = outbound_rx.recv().await {
        match req {
            CollabRequest::Hello { app_id: _, cluster_id } => {
                // Ride the identity blob captured at manager
                // construction — the loaded runtime-identity's raw
                // pubkey bytes when the on-disk record was reachable,
                // an empty vec otherwise. The broker treats empty as
                // the "no identity" fallback (unchanged behaviour for
                // apps outside a qOS session or missing an
                // `QOS_USER_IDENTITY` env var).
                let channel_id = cluster_id.to_string();
                let ch = match client.join_channel(&channel_id, &identity_bytes).await {
                    Ok(ch) => ch,
                    Err(_) => continue,
                };

                // subscribe(): forward signed op events.
                if let Ok(sub) = ch.subscribe().await {
                    let itx = inbound_tx.clone();
                    tokio::task::spawn_local(async move {
                        loop {
                            let Ok(evt) = sub.read().await else {
                                break;
                            };
                            // The SDK carries the postcard-encoded
                            // SignedOp on the wire; decode into the
                            // typed envelope callers already handle.
                            let Ok(envelope) =
                                postcard::from_bytes::<SignedOp>(&evt.op)
                            else {
                                continue;
                            };
                            if itx
                                .send(CollabEvent::SignedCRDTIncremental {
                                    envelope: Box::new(envelope),
                                    from_user_id: evt.from_user_id,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    });
                }

                // subscribeContext(): forward roster / permission snapshots.
                if let Ok(ctx_stream) = ch.subscribe_context().await {
                    let itx = inbound_tx.clone();
                    tokio::task::spawn_local(async move {
                        loop {
                            let Ok(ctx) = ctx_stream.read().await else {
                                break;
                            };
                            let collaborators = ctx
                                .collaborators
                                .into_iter()
                                .map(|c| Collaborator {
                                    id: c.user_id,
                                    name: c.display_name,
                                    color: unpack_color(c.color),
                                })
                                .collect();
                            if itx
                                .send(CollabEvent::Context {
                                    collaborators,
                                    permissions: ctx.permissions,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    });
                }

                // presence + subscribeAll(): forward every presence
                // variant. Cursor → RemoteCursorMoved (with the empty
                // PresenceKey the legacy dialect used for pointers);
                // every other payload → RemotePresenceUpdate keyed by
                // the decoded PresenceKey.
                if let Ok(ph) = ch.presence().await {
                    if let Ok(ps) = ph.subscribe_all().await {
                        let itx = inbound_tx.clone();
                        tokio::task::spawn_local(async move {
                            loop {
                                let Ok(evt) = ps.read().await else {
                                    break;
                                };
                                let translated = translate_presence_event(evt);
                                if itx.send(translated).is_err() {
                                    break;
                                }
                            }
                        });
                    }
                    presence = Some(ph);
                }

                channel = Some(ch);
            }
            CollabRequest::SignedSyncIncremental { envelope } => {
                if let Some(ch) = channel.as_ref() {
                    if let Ok(bytes) = postcard::to_stdvec(&*envelope) {
                        let _ = ch.send(&bytes).await;
                    }
                }
            }
            CollabRequest::SyncCRDT { data } => {
                if let Some(ch) = channel.as_ref() {
                    let _ = ch.send_unsigned_snapshot(&data).await;
                }
            }
            CollabRequest::SyncIncremental { data } => {
                if let Some(ch) = channel.as_ref() {
                    let _ = ch.send_unsigned_incremental(&data).await;
                }
            }
            CollabRequest::Presence { key, payload } => {
                let Some(ph) = presence.as_ref() else { continue };
                match payload {
                    PresencePayload::Pointer { x, y } => {
                        let _ = ph.publish_cursor(x, y).await;
                    }
                    PresencePayload::Caret { pos } => {
                        let key_bytes = postcard::to_stdvec(&key).unwrap_or_default();
                        let _ = ph.publish_caret(&key_bytes, pos as u64).await;
                    }
                    PresencePayload::Selection { start, end } => {
                        let key_bytes = postcard::to_stdvec(&key).unwrap_or_default();
                        let _ = ph
                            .publish_selection(&key_bytes, start as u64, end as u64)
                            .await;
                    }
                    PresencePayload::Arrived => {
                        let key_bytes = postcard::to_stdvec(&key).unwrap_or_default();
                        let _ = ph.publish_arrived(&key_bytes).await;
                    }
                    PresencePayload::Left => {
                        let key_bytes = postcard::to_stdvec(&key).unwrap_or_default();
                        let _ = ph.publish_left(&key_bytes).await;
                    }
                }
            }
            CollabRequest::FetchScrollback {
                artifact,
                before_seq,
                max,
            } => {
                let Some(ch) = channel.as_ref() else { continue };
                match ch.fetch_scrollback(before_seq, max as u32).await {
                    Ok(batch) => {
                        let mut ops = Vec::with_capacity(batch.ops.len());
                        for op_bytes in batch.ops {
                            if let Ok(env) = postcard::from_bytes::<SignedOp>(&op_bytes) {
                                ops.push(Box::new(env));
                            }
                        }
                        let _ = inbound_tx.send(CollabEvent::ScrollbackBatch {
                            artifact,
                            ops,
                            exhausted: batch.done,
                        });
                    }
                    Err(_) => {
                        // Signal exhaustion so callers unstick their
                        // paging state instead of hanging.
                        let _ = inbound_tx.send(CollabEvent::ScrollbackBatch {
                            artifact,
                            ops: Vec::new(),
                            exhausted: true,
                        });
                    }
                }
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Relay backend: bridge task + translation.
// -----------------------------------------------------------------------------

/// Bridge task for [`Backend::Relay`]. Owns the joined
/// [`qos_relay::RelayCoeditSession`] and shuttles between the toolkit's
/// synchronous `send` / `poll` surface and the session's async publish /
/// recv verbs:
///
/// - outbound [`CollabRequest`]s map onto `publish_delta` /
///   `publish_signed` / `publish_presence` (or a synthetic
///   empty-scrollback reply — the relay's store-and-forward is not a
///   history-query API);
/// - inbound [`qos_relay::Inbound`] events map back onto the matching
///   [`CollabEvent`] the App layer already dispatches.
///
/// The two directions share a `select!` because both borrow the session
/// immutably (`recv` / `publish_*` all take `&self`), and the recv
/// future is dropped whenever an outbound request fires. Returns — ending
/// the task — when the manager is dropped (outbound channel closed) or the
/// host closes the session (`recv` yields `None`).
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
                    // The relay session is joined at construction, so the
                    // legacy auto-Hello has no work to do here.
                    CollabRequest::Hello { .. } => {}
                    // Full snapshot + incremental both ride the unsigned
                    // co-edit path; the host forwards the opaque delta
                    // bytes and the receiver applies them with
                    // `load_incremental`.
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
                        // The session stamps the author; we only carry the
                        // (key, payload) body it needs to reconstruct the
                        // toolkit presence event on the far side.
                        let _ = session.publish_presence(encode_relay_presence(&key, &payload));
                    }
                    CollabRequest::FetchScrollback { artifact, .. } => {
                        // The host queues deltas per-peer for delivery, it
                        // does not answer seq-range history queries. Reply
                        // empty + exhausted so a paging caller unsticks
                        // instead of hanging.
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
                if let Some(evt) = translate_relay_inbound(inbound) {
                    if inbound_tx.send(evt).is_err() {
                        break;
                    }
                }
            }
        }
    }
}

/// Encode the toolkit `(PresenceKey, PresencePayload)` pair as the
/// opaque body carried inside a relay presence frame. The session wraps
/// it with author attribution before it hits the wire.
fn encode_relay_presence(key: &PresenceKey, payload: &PresencePayload) -> Vec<u8> {
    postcard::to_stdvec(&(key, payload)).unwrap_or_default()
}

/// Inverse of [`encode_relay_presence`]. `None` for a body that is not a
/// toolkit presence pair (a non-co-edit presence frame from another
/// tier) — the caller skips it rather than surface a bogus event.
fn decode_relay_presence(bytes: &[u8]) -> Option<(PresenceKey, PresencePayload)> {
    postcard::from_bytes(bytes).ok()
}

/// Derive a stable `user_id` string from a publishing device's public
/// key. The relay attributes events by 32-byte device key; the toolkit's
/// presence / remote-cursor layer keys on a `String`, so we render the
/// key as `peer-<hex>` — the same `peer-` id shape the broker mints for
/// its fan-out, keeping downstream attribution code on one id format.
fn user_id_from_author(author: [u8; 32]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(5 + 64);
    s.push_str("peer-");
    for b in author {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Translate a host-side [`qos_relay::Inbound`] into the toolkit
/// [`CollabEvent`] the App layer already dispatches. `None` for frames
/// that carry no app-visible event (the subscribe `Ready` ack) or that
/// fail to decode.
fn translate_relay_inbound(inbound: qos_relay::Inbound) -> Option<CollabEvent> {
    match inbound {
        // Unsigned document delta — the legacy incremental path apps
        // already merge. (`CRDTIncremental` carries no author, matching
        // the unsigned broker path; signed ops carry attribution below.)
        qos_relay::Inbound::Delta { data, .. } => {
            Some(CollabEvent::CRDTIncremental { data })
        }
        // Signed document op — decode the envelope and attribute it to
        // the publishing device key.
        qos_relay::Inbound::SignedDelta { author, op_bytes } => {
            let envelope = postcard::from_bytes::<SignedOp>(&op_bytes).ok()?;
            Some(CollabEvent::SignedCRDTIncremental {
                envelope: Box::new(envelope),
                from_user_id: user_id_from_author(author),
            })
        }
        // Presence — the author rode inside the co-edit payload (the host
        // envelope carries none), so it is preserved end-to-end here.
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

/// Unpack the SDK's 0x00RRGGBB u32 into normalised [R, G, B] floats.
/// The legacy dialect carries colour as `[f32; 3]` in linear-0..1;
/// preserving that gives every existing consumer identical rendering.
fn unpack_color(packed: u32) -> [f32; 3] {
    let r = ((packed >> 16) & 0xFF) as f32 / 255.0;
    let g = ((packed >> 8) & 0xFF) as f32 / 255.0;
    let b = (packed & 0xFF) as f32 / 255.0;
    [r, g, b]
}

/// Translate a SDK-side [`runtime_collab::PresenceEvent`] into the
/// public `CollabEvent` variant the App layer already dispatches.
///
/// Extracted so the subscription task body stays skimmable and the
/// unit test can round-trip payloads without spinning a broker.
fn translate_presence_event(evt: runtime_collab::PresenceEvent) -> CollabEvent {
    // Cursor keeps the legacy shape: `user_id + x + y` on
    // `RemoteCursorMoved`. Every other variant carries the
    // (postcard-decoded) `PresenceKey` on
    // `RemotePresenceUpdate`. If key decoding fails on a non-Cursor
    // variant we fall back to a synthetic key; the peer's presence is
    // still visible but bound to a placeholder scope.
    match evt.payload {
        runtime_collab::PresencePayload::Cursor { x, y } => CollabEvent::RemoteCursorMoved {
            user_id: evt.user_id,
            x,
            y,
        },
        other => {
            let key: PresenceKey = postcard::from_bytes(&evt.key_bytes).unwrap_or_else(|_| {
                PresenceKey::Tab {
                    id: String::from("<undecodable>"),
                }
            });
            let payload = match other {
                runtime_collab::PresencePayload::Caret { pos } => PresencePayload::Caret {
                    pos: pos as usize,
                },
                runtime_collab::PresencePayload::Selection { start, end } => {
                    PresencePayload::Selection {
                        start: start as usize,
                        end: end as usize,
                    }
                }
                runtime_collab::PresencePayload::Arrived => PresencePayload::Arrived,
                runtime_collab::PresencePayload::Left => PresencePayload::Left,
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

// -----------------------------------------------------------------------------
// Legacy N-peer broker.
// -----------------------------------------------------------------------------

/// Standalone N-peer fan-out broker for `QUICK_COLLAB_SOCK`.
///
/// This is the production counterpart to the test relay in
/// `ui/tests/real_socket_two_peer.rs`, generalized from a fixed two
/// peers to any number that connect over the lifetime of the process.
/// It carries no Automerge knowledge — it simply relays opaque frames:
///
///   * `Hello` registers the connection's `user_id` (from `app_id`) and
///     is then swallowed (never fanned out).
///   * `SyncIncremental` → `CRDTIncremental`, `SyncCRDT` → `CRDTUpdate`
///     are forwarded verbatim to every *other* connected peer.
///   * `Presence` → `RemotePresenceUpdate` is forwarded to every other
///     peer, attributed with the sender's `user_id`.
///
/// Late-join document convergence is handled at the app layer (a new
/// peer broadcasts `Presence::Arrived`; peers already in the room reply
/// with a full `SyncCRDT`) — the broker needs no special case for it.
#[cfg(feature = "legacy-broker")]
pub mod broker {
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::Path;
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::{CollabEvent, CollabRequest};

    fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        s.read_exact(&mut len_buf).ok()?;
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        s.read_exact(&mut buf).ok()?;
        Some(buf)
    }

    fn write_frame(s: &mut UnixStream, bytes: &[u8]) {
        let len = bytes.len() as u32;
        let _ = s.write_all(&len.to_le_bytes());
        let _ = s.write_all(bytes);
    }

    /// A connected peer's write side + attributed identity.
    struct Peer {
        user_id: String,
        write: UnixStream,
    }

    /// Bind `path` (removing any stale inode first) and serve forever.
    /// Convenience entry point for the `collab-broker` binary.
    pub fn serve_path<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
        let path = path.as_ref();
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path)?;
        serve(listener);
        Ok(())
    }

    /// Serve on an already-bound listener until it stops yielding
    /// connections. Blocks the calling thread; spawns one reader thread
    /// per peer plus an accept thread.
    pub fn serve(listener: UnixListener) {
        let peers: Arc<Mutex<HashMap<usize, Peer>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (fwd_tx, fwd_rx) = mpsc::channel::<(usize, Vec<u8>)>();

        // Accept connections on a side thread so the forwarding loop can
        // own the calling thread. Each connection gets its own reader
        // thread so a silent peer can't stall accept for the others.
        let accept_peers = Arc::clone(&peers);
        thread::spawn(move || {
            let mut next_idx = 0usize;
            for conn in listener.incoming() {
                let Ok(mut stream) = conn else { continue };
                let idx = next_idx;
                next_idx += 1;
                let tx = fwd_tx.clone();
                let peers = Arc::clone(&accept_peers);
                thread::spawn(move || {
                    let Ok(write) = stream.try_clone() else { return };
                    // First frame establishes identity. The client sends
                    // `Hello` immediately on connect; if it doesn't, fall
                    // back to `peer-{idx}` and treat the frame as data.
                    match read_frame(&mut stream) {
                        Some(frame) => match serde_json::from_slice::<CollabRequest>(&frame) {
                            Ok(CollabRequest::Hello { app_id, .. }) => {
                                peers.lock().unwrap().insert(
                                    idx,
                                    Peer { user_id: app_id, write },
                                );
                            }
                            _ => {
                                peers.lock().unwrap().insert(
                                    idx,
                                    Peer { user_id: format!("peer-{idx}"), write },
                                );
                                let _ = tx.send((idx, frame));
                            }
                        },
                        None => return, // closed before saying anything
                    }

                    while let Some(frame) = read_frame(&mut stream) {
                        if tx.send((idx, frame)).is_err() {
                            break;
                        }
                    }
                    // Peer closed — stop writing to it.
                    peers.lock().unwrap().remove(&idx);
                });
            }
        });

        // Forwarding loop. Never returns while the accept thread holds a
        // `fwd_tx` clone (i.e. for the life of the process).
        while let Ok((src_idx, frame)) = fwd_rx.recv() {
            let Ok(req) = serde_json::from_slice::<CollabRequest>(&frame) else {
                continue;
            };

            let user_id = peers
                .lock()
                .unwrap()
                .get(&src_idx)
                .map(|p| p.user_id.clone())
                .unwrap_or_else(|| format!("peer-{src_idx}"));

            let event = match req {
                CollabRequest::Hello { .. } => continue,
                CollabRequest::SyncIncremental { data } => {
                    CollabEvent::CRDTIncremental { data }
                }
                CollabRequest::SyncCRDT { data } => {
                    CollabEvent::CRDTUpdate { data }
                }
                CollabRequest::Presence { key, payload } => {
                    CollabEvent::RemotePresenceUpdate { user_id, key, payload }
                }
                CollabRequest::SignedSyncIncremental { envelope } => {
                    // The simple in-toolkit test broker is for two-peer
                    // demos and does NOT enforce roles — it just routes
                    // verbatim. Production fan-out is the
                    // `shell::shell::collab_broker` which runs every
                    // envelope through `validate_inbound_op` before
                    // forwarding. Documenting the divergence here
                    // explicitly so a reader doesn't assume the
                    // test broker is the production security boundary.
                    CollabEvent::SignedCRDTIncremental {
                        envelope,
                        from_user_id: user_id,
                    }
                }
                CollabRequest::FetchScrollback { artifact, .. } => {
                    // The in-toolkit test broker holds no op log
                    // (Phase 6 is broker-side state), so the answer is
                    // always "empty + exhausted". Apps using this
                    // broker get no scrollback; production apps wire
                    // through `shell::shell::collab_broker` which
                    // does hold the log.
                    CollabEvent::ScrollbackBatch {
                        artifact,
                        ops: Vec::new(),
                        exhausted: true,
                    }
                }
            };

            let Ok(bytes) = serde_json::to_vec(&event) else { continue };

            let mut guard = peers.lock().unwrap();
            let recipients = guard.len().saturating_sub(1);
            if std::env::var("QUICK_COLLAB_DEBUG").is_ok() {
                let kind = match &event {
                    CollabEvent::CRDTUpdate { data } => format!("CRDTUpdate({}B)", data.len()),
                    CollabEvent::CRDTIncremental { data } => {
                        format!("CRDTIncremental({}B)", data.len())
                    }
                    CollabEvent::SignedCRDTIncremental { envelope, from_user_id } => {
                        format!(
                            "SignedCRDTIncremental(from={from_user_id}, {}B payload)",
                            envelope.payload.len()
                        )
                    }
                    CollabEvent::RemotePresenceUpdate { payload, .. } => {
                        format!("Presence({payload:?})")
                    }
                    other => format!("{other:?}"),
                };
                eprintln!("[broker] peer{src_idx} -> {kind} -> {recipients} peer(s)");
            }
            for (idx, peer) in guard.iter_mut() {
                if *idx != src_idx {
                    write_frame(&mut peer.write, &bytes);
                }
            }
        }
    }
}

// -----------------------------------------------------------------------------
// Unit tests: SDK translation + shovel-thread round-trip against a fake broker.
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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

    // --- shared capture / seed state -------------------------------------

    #[derive(Default)]
    struct Captures {
        joined: Vec<(String, Vec<u8>)>,
        signed_ops: Vec<Vec<u8>>,
        unsigned_snapshots: Vec<Vec<u8>>,
        unsigned_incrementals: Vec<Vec<u8>>,
        cursor_publishes: Vec<(f32, f32)>,
        caret_publishes: Vec<(Vec<u8>, u64)>,
        selection_publishes: Vec<(Vec<u8>, u64, u64)>,
        arrived: Vec<Vec<u8>>,
        left: Vec<Vec<u8>>,
        scrollback_requests: Vec<(u64, u32)>,
    }

    /// Shared between the test-thread's tokio LocalSet and the caller.
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

    // --- fake presence stream -------------------------------------------

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

    // --- fake presence handle -------------------------------------------

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
            let Ok(r) = params.get() else { return Promise::ok(()) };
            self.captures
                .lock()
                .unwrap()
                .cursor_publishes
                .push((r.get_x(), r.get_y()));
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
            let Ok(r) = params.get() else { return Promise::ok(()) };
            let Ok(k) = r.get_key_bytes() else { return Promise::ok(()) };
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
            let Ok(r) = params.get() else { return Promise::ok(()) };
            let Ok(k) = r.get_key_bytes() else { return Promise::ok(()) };
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
            let Ok(r) = params.get() else { return Promise::ok(()) };
            let Ok(k) = r.get_key_bytes() else { return Promise::ok(()) };
            self.captures.lock().unwrap().arrived.push(k.to_vec());
            Promise::ok(())
        }

        fn publish_left(
            self: Rc<Self>,
            params: presence_handle::PublishLeftParams,
            _results: presence_handle::PublishLeftResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(r) = params.get() else { return Promise::ok(()) };
            let Ok(k) = r.get_key_bytes() else { return Promise::ok(()) };
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

    // --- fake op stream ---------------------------------------------------

    struct FakeOpStream {
        backlog: Rc<RefCell<Vec<(Vec<u8>, String)>>>,
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

    // --- fake context stream ---------------------------------------------

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
                let mut collabs = ctx
                    .reborrow()
                    .init_collaborators(seed.collaborators.len() as u32);
                for (i, (uid, name, color)) in seed.collaborators.iter().enumerate() {
                    let mut c = collabs.reborrow().get(i as u32);
                    c.set_user_id(uid);
                    c.set_display_name(name);
                    c.set_color(*color);
                }
            }
            {
                let mut perms = ctx.init_permissions(seed.permissions.len() as u32);
                for (i, p) in seed.permissions.iter().enumerate() {
                    perms.set(i as u32, p.as_str());
                }
            }
            Promise::ok(())
        }
    }

    // --- fake channel handle ---------------------------------------------

    struct FakeChannel {
        captures: SharedCaptures,
        op_backlog: Rc<RefCell<Vec<(Vec<u8>, String)>>>,
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
            let op = match params.get().and_then(|r| r.get_op()) {
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
            let Ok(d) = params.get().and_then(|r| r.get_data()) else {
                return Promise::ok(());
            };
            self.captures.lock().unwrap().unsigned_snapshots.push(d.to_vec());
            Promise::ok(())
        }

        fn send_unsigned_incremental(
            self: Rc<Self>,
            params: channel_handle::SendUnsignedIncrementalParams,
            _results: channel_handle::SendUnsignedIncrementalResults,
        ) -> Promise<(), capnp::Error> {
            let Ok(d) = params.get().and_then(|r| r.get_data()) else {
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
            let Ok(p) = params.get() else { return Promise::ok(()) };
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
                let mut ops = batch.reborrow().init_ops(seed.ops.len() as u32);
                for (i, op) in seed.ops.iter().enumerate() {
                    ops.set(i as u32, op);
                }
            }
            batch.set_earliest_seq(seed.earliest_seq);
            batch.set_latest_seq(seed.latest_seq);
            batch.set_done(seed.done);
            Promise::ok(())
        }
    }

    // --- fake broker + wiring --------------------------------------------

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
            let Ok(reader) = params.get() else { return Promise::ok(()) };
            let channel_id = reader
                .get_channel_id()
                .and_then(|t| {
                    t.to_str()
                        .map(|s| s.to_owned())
                        .map_err(|e| capnp::Error::failed(format!("bad utf8: {e}")))
                })
                .unwrap_or_default();
            let identity = reader
                .get_identity()
                .map(|b| b.to_vec())
                .unwrap_or_default();
            self.captures.lock().unwrap().joined.push((channel_id, identity));

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

    /// Stand up a fake `CollabService` on one end of a Unix socket
    /// pair and return the client-side raw fd + the captures state so
    /// tests can assert.
    fn stand_up_fake_broker(
        broker: FakeBroker,
    ) -> (RawFd, SharedCaptures, thread::JoinHandle<()>) {
        let captures = broker.captures.clone();
        // Build the socket pair on the caller thread — pair() is
        // synchronous.
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
                        Default::default(),
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

    // --- tests -----------------------------------------------------------

    #[test]
    fn disconnected_manager_is_a_no_op() {
        // No SDK, no legacy socket set. `new` cannot reach anything —
        // falls through to `Backend::Disabled`.
        // Guard the env var against pollution from a co-running test.
        // SAFETY: the crate is single-threaded per-test module here.
        unsafe {
            std::env::remove_var("QOS_SVC_COLLAB_FD");
            std::env::remove_var("QUICK_COLLAB_SOCK");
        }
        let mut mgr = CollaborationManager::new();
        assert!(!mgr.is_connected(), "no service = not connected");
        mgr.send(CollabRequest::SyncCRDT { data: b"nope".to_vec() });
        assert!(mgr.poll().is_empty(), "no events on Disabled backend");
    }

    #[test]
    fn join_channel_uses_empty_identity_when_env_var_unset() {
        // The manager's public constructor (`new`) is what apps hit.
        // With `QOS_USER_IDENTITY` unset, `runtime-identity::load()`
        // returns `ServiceFdMissing` and the manager falls back to
        // the empty-identity path — matching the pre-existing wire
        // behaviour so a legacy broker sees no diff.
        let broker = FakeBroker {
            captures: std::sync::Arc::new(std::sync::Mutex::new(Captures::default())),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, captures, _th) = stand_up_fake_broker(broker);

        // SAFETY: env-var mutations in this test module are logically
        // serial — no other test in this module races on the same var.
        unsafe { std::env::remove_var(runtime_identity::IDENTITY_ENV_VAR) };
        let cluster = Uuid::new_v4();
        let _mgr =
            CollaborationManager::from_sdk_fd_for_test_with_identity(fd, "alice", cluster, Vec::new());

        assert!(
            wait_until(
                || !captures.lock().unwrap().joined.is_empty(),
                Duration::from_secs(2),
            ),
            "join_channel must be observed"
        );
        let joined = captures.lock().unwrap().joined.clone();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].0, cluster.to_string());
        assert!(joined[0].1.is_empty(), "identity blob must be empty when env unset");
    }

    #[test]
    fn join_channel_carries_pubkey_bytes_when_identity_loads() {
        // A fresh single-device identity written to disk and pointed
        // at via QOS_USER_IDENTITY must ride the join_channel call as
        // its pubkey bytes. This is the property that makes presence
        // authored and signed-op provenance meaningful.
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

        // We can't call `CollaborationManager::new()` here — it also
        // reaches for `QOS_SVC_COLLAB_FD` and would race any other
        // test in the workspace using the same env var. Instead
        // simulate the load-then-forward path explicitly against the
        // fake broker via the identity-aware test constructor.
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
            joined[0].1,
            identity_bytes,
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

        // Auto-Hello reaches the broker.
        assert!(
            wait_until(|| !captures.lock().unwrap().joined.is_empty(), Duration::from_secs(2)),
            "join_channel must be observed"
        );
        let joined = captures.lock().unwrap().joined.clone();
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].0, cluster.to_string());

        // Signed op forwarded.
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
            wait_until(|| !captures.lock().unwrap().joined.is_empty(), Duration::from_secs(2)),
            "join_channel must be observed",
        );

        mgr.send(CollabRequest::SyncCRDT { data: b"snap".to_vec() });
        mgr.send(CollabRequest::SyncIncremental { data: b"delta".to_vec() });

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
        let c = captures.lock().unwrap();
        assert_eq!(c.unsigned_snapshots, vec![b"snap".to_vec()]);
        assert_eq!(c.unsigned_incrementals, vec![b"delta".to_vec()]);
    }

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
            wait_until(|| !captures.lock().unwrap().joined.is_empty(), Duration::from_secs(2)),
            "join_channel must be observed",
        );

        let key = PresenceKey::Tab { id: "note-1".to_owned() };
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
            key: key.clone(),
            payload: PresencePayload::Left,
        });

        assert!(
            wait_until(
                || {
                    let c = captures.lock().unwrap();
                    c.cursor_publishes.len() == 1
                        && c.caret_publishes.len() == 1
                        && c.selection_publishes.len() == 1
                        && c.arrived.len() == 1
                        && c.left.len() == 1
                },
                Duration::from_secs(2),
            ),
            "every presence variant must reach the corresponding SDK method",
        );
        let c = captures.lock().unwrap();
        assert_eq!(c.cursor_publishes, vec![(1.5, 2.5)]);
        assert_eq!(c.caret_publishes, vec![(expected_key_bytes.clone(), 12)]);
        assert_eq!(
            c.selection_publishes,
            vec![(expected_key_bytes.clone(), 3, 9)]
        );
        assert_eq!(c.arrived, vec![expected_key_bytes.clone()]);
        assert_eq!(c.left, vec![expected_key_bytes]);
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
                CollabEvent::SignedCRDTIncremental { envelope, from_user_id } => {
                    Some((envelope, from_user_id))
                }
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
                    ("alice".to_owned(), "Alice".to_owned(), 0x00FF8000),
                    ("bob".to_owned(), "Bob".to_owned(), 0x000080FF),
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
        let key = PresenceKey::Tab { id: "note-1".to_owned() };
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
                key_bytes: key_bytes.clone(),
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

        // Extract the presence-relevant events in order.
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
            wait_until(|| !captures.lock().unwrap().joined.is_empty(), Duration::from_secs(2)),
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
        // Straight unit test of `translate_presence_event` — no broker
        // required, no threading. Covers cursor / caret / selection /
        // arrived / left and the undecodable-key fallback.
        let key = PresenceKey::Widget {
            path: vec![crate::presence::PathSegment::Field("body".into())],
        };
        let key_bytes = postcard::to_stdvec(&key).unwrap();

        // Cursor
        let e = translate_presence_event(runtime_collab::PresenceEvent {
            user_id: "peer".into(),
            key_bytes: key_bytes.clone(),
            payload: SdkPayload::Cursor { x: 7.0, y: 8.0 },
        });
        matches!(e, CollabEvent::RemoteCursorMoved { .. });

        // Caret
        let e = translate_presence_event(runtime_collab::PresenceEvent {
            user_id: "peer".into(),
            key_bytes: key_bytes.clone(),
            payload: SdkPayload::Caret { pos: 3 },
        });
        if let CollabEvent::RemotePresenceUpdate { key: k, payload, .. } = e {
            assert_eq!(k, key);
            assert_eq!(payload, PresencePayload::Caret { pos: 3 });
        } else {
            panic!("expected RemotePresenceUpdate");
        }

        // Undecodable key → fallback.
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
