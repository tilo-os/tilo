//! `tilo-relay` — the Tilo host + relay daemon.
//!
//! # Model
//!
//! A **host** in Tilo is a device that (a) presents a `DeviceCert`
//! chaining to your user identity, (b) is marked always-on, and (c)
//! pins one or more workspaces so their `WorkspaceDirectory` CRDT is
//! held persistently even while your laptop sleeps. The same daemon
//! also **relays** — when your phone and laptop cannot punch through
//! each other's NATs, they both hole-punch to a host and exchange
//! CRDT deltas through it; when one is offline, its deltas are
//! **store-and-forwarded** on disk until it reconnects.
//!
//! # Wire model
//!
//! The daemon binds a single iroh endpoint. Every inbound
//! connection speaks a Tilo relay ALPN and performs a small
//! authenticated handshake ([`transport::hello`]):
//!
//! 1. Client sends its `DeviceCert` + a challenge nonce.
//! 2. Server verifies the cert chains to *this relay's* user
//!    identity, replies with its own cert + a nonce + an Ed25519
//!    signature over the client's nonce (challenge/response).
//! 3. Client verifies the server's cert + signature, then sends the
//!    server a signature of its own over the server's nonce.
//! 4. Both sides are now authenticated: mutual proof of possession
//!    of a device signing key that chains to the same user identity.
//!
//! After the handshake, the connection is a bidirectional pipe of
//! framed [`transport::Envelope`] messages: `Subscribe(workspace_id)`,
//! `Delta(workspace_id, bytes)`, `Presence(workspace_id, bytes)`,
//! `Ack(seq)`, `RequestPending(workspace_id)`. The daemon receives
//! deltas, appends them to the workspace's on-disk log, and fans
//! them out to every other peer currently subscribed to the same
//! workspace. Offline peers accumulate deltas in
//! `workspaces/<id>/pending/<peer_id>/`.
//!
//! # Multi-host
//!
//! Two relays bound to the same workspace are peers in the same
//! Automerge mesh — from the CRDT's point of view they are
//! indistinguishable from any other member device. The workspace-
//! directory carries a `Vec<[u8; 32]>` of host device pubkeys per
//! entry (see [`tilo_user_identity::WorkspaceMeta::host_devices`]);
//! peers pick the nearest available host and fail over on timeout.
//!
//! # Current boundaries
//!
//! - App backends are not executed here. Compute packages run only through
//!   [`compute::ComputeWorker`], which binds certified routes and content
//!   digests to the normal signed-package launcher sandbox.
//! - No content byte transfer — content hydration continues to
//!   travel over `iroh-blobs` inside the `crates/system/fs/` crate; the relay
//!   handles only the CRDT-directory tier.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(feature = "agents")]
pub mod agents;
#[cfg(not(target_arch = "wasm32"))]
pub mod app_serving;
#[cfg(not(target_arch = "wasm32"))]
mod authority_epoch;
#[cfg(not(target_arch = "wasm32"))]
pub mod certification;
#[cfg(not(target_arch = "wasm32"))]
pub mod compaction;
#[cfg(not(target_arch = "wasm32"))]
pub mod compute;
#[cfg(not(target_arch = "wasm32"))]
pub mod compute_session;
#[cfg(not(target_arch = "wasm32"))]
pub mod compute_transport;
#[cfg(not(target_arch = "wasm32"))]
pub mod compute_worker;
#[cfg(not(target_arch = "wasm32"))]
pub mod daemon;
#[cfg(not(target_arch = "wasm32"))]
pub mod environment_bridge;
#[cfg(target_os = "linux")]
pub mod environment_guest;
#[cfg(not(target_arch = "wasm32"))]
pub mod environment_host;
#[cfg(not(target_arch = "wasm32"))]
pub mod environment_routing;
#[cfg(not(target_arch = "wasm32"))]
pub mod environment_tools;
#[cfg(not(target_arch = "wasm32"))]
mod handshake;
#[cfg(not(target_arch = "wasm32"))]
pub mod legacy_host;
#[cfg(target_os = "linux")]
pub mod legacy_route;
#[cfg(target_os = "linux")]
pub mod legacy_vm_host;
#[cfg(not(target_arch = "wasm32"))]
mod membership_watch;
#[cfg(not(target_arch = "wasm32"))]
pub mod metrics;
#[cfg(not(target_arch = "wasm32"))]
pub mod peer;
#[cfg(not(target_arch = "wasm32"))]
mod persistence;
#[cfg(not(target_arch = "wasm32"))]
pub mod presence_mailbox;
#[cfg(not(target_arch = "wasm32"))]
pub mod relay_offers;
#[cfg(not(target_arch = "wasm32"))]
mod room_access;
#[cfg(not(target_arch = "wasm32"))]
pub mod room_records;
#[cfg(not(target_arch = "wasm32"))]
pub mod rooms;
#[cfg(not(target_arch = "wasm32"))]
pub mod store;
#[cfg(not(target_arch = "wasm32"))]
pub mod surfaces;
#[cfg(feature = "web")]
pub mod web;
#[cfg(not(target_arch = "wasm32"))]
pub mod workspace;
#[cfg(not(target_arch = "wasm32"))]
mod workspace_io;
pub mod workspace_join;

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
tilo_automerge_isolate::test_worker!();
#[cfg(all(test, feature = "agents"))]
tilo_agent_participant::app_worker!();

// The records and wire messages shared with relay clients live in
// `tilo-relay-protocol`, the frame codec in `tilo-relay-client`, and the
// session clients (relay, co-edit, anti-entropy, room administration and
// workspace join) in `tilo-relay-session`, so clients need not link the
// daemon. The daemon keeps addressing them here.
pub use tilo_relay_client::frame;
pub use tilo_relay_protocol::{
    auth, certify, config, home_peer, live, mesh, policy, process_lease,
};
pub use tilo_relay_session::{antientropy, client, coedit, position};

/// The relay wire messages ([`tilo_relay_protocol::transport`]) together with
/// the envelope frame codec ([`frame`]), under the path the daemon and its
/// callers have always used for both.
pub mod transport {
    pub use crate::frame::{FrameError, FrameReader, decode, encode};
    pub use tilo_relay_protocol::transport::*;
}

pub use antientropy::{SyncError, apply_sync_response, heads_wire, reconcile, send_sync_request};
#[cfg(not(target_arch = "wasm32"))]
pub use app_serving::{ActionHostPlane, AppServingPlane};
pub use auth::PeerRole;
pub use client::{ClientError, RelayClient};
pub use coedit::{AttributedPresence, CoeditError, CoeditPayload, Inbound, RelayCoeditSession};
#[cfg(not(target_arch = "wasm32"))]
pub use compute::{
    ComputeContentStore, ComputeError, ComputeExecutionError, ComputeJobLifecycle, ComputeJobPhase,
    ComputeJobResult, ComputeJobSpec, ComputePackageCatalogue, ComputeResourceLimits,
    ComputeSandbox, ComputeSandboxInvocation, ComputeWorker, InstalledComputeCatalogue,
    LauncherComputeSandbox, admit_compute_job,
};
#[cfg(not(target_arch = "wasm32"))]
pub use compute_session::{ComputeClient, ComputeDelivery, ComputeSessionError};
#[cfg(not(target_arch = "wasm32"))]
pub use compute_transport::{
    ComputeEnvelope, ComputeJobBinding, ComputeResultUpdate, ComputeRouteOutcome,
    ComputeStatusUpdate, ComputeTransportError, ComputeWorkerAdvertisement,
};
#[cfg(not(target_arch = "wasm32"))]
pub use compute_worker::{
    ComputeWorkerError, FileComputeContentStore, ProductionComputeWorkerConfig, WorkerEnvironments,
    run_production_compute_worker, serve_worker_stream, spawn_worker_stream,
};
pub use config::{CheckpointPrefixRecovery, PeerRelayConfig, RelayConfig, WorkspaceBinding};
#[cfg(not(target_arch = "wasm32"))]
pub use daemon::{Relay, RelayError, RelayHandle};
pub use home_peer::{HomePeerRegistration, home_peer_registry_path, valid_home_peer_label};
pub use mesh::{
    ComputeAccelerator, DeviceMeshProfile, DeviceRole, PowerAvailability, ReplicationIntent,
    ReplicationResource, ServerAdvertisement, ServiceCapabilities,
};
pub use policy::TrustPolicy;
pub use process_lease::ProcessLease;
pub use transport::ContentPeerRegistration;
pub use transport::{ALPN, Envelope};

/// Semantic version emitted by `tilo-relay --version`. Tied to the
/// crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use tilo_relay_protocol::{
    personal_continuity_workspace_id, personal_continuity_workspace_id_matches, room_workspace_id,
    room_workspace_id_matches,
};
