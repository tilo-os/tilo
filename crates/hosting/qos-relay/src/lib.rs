//! `qos-relay` — the qOS host + relay daemon.
//!
//! # Model
//!
//! A **host** in qOS is a device that (a) presents a `DeviceCert`
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
//! connection speaks the `qos-relay/1` ALPN and performs a small
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
//! entry (see [`qos_user_identity::WorkspaceMeta::host_devices`]);
//! peers pick the nearest available host and fail over on timeout.
//!
//! # Current boundaries
//!
//! - The daemon does not yet execute app or compute jobs. [`mesh`] defines the
//!   capability-bounded placement seam so later workers share host selection,
//!   identity and workspace authorization instead of becoming a second grid.
//! - No content byte transfer — content hydration continues to
//!   travel over `iroh-blobs` inside the `fs/` crate; the relay
//!   handles only the CRDT-directory tier.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod antientropy;
pub mod auth;
pub mod client;
pub mod coedit;
pub mod config;
pub mod daemon;
pub mod home_peer;
pub mod mesh;
pub mod metrics;
pub mod peer;
pub mod policy;
pub mod process_lease;
pub mod store;
pub mod transport;
pub mod workspace;

pub use antientropy::{SyncError, apply_sync_response, heads_wire, reconcile, send_sync_request};
pub use auth::PeerRole;
pub use client::{ClientError, RelayClient};
pub use coedit::{AttributedPresence, CoeditError, CoeditPayload, Inbound, RelayCoeditSession};
pub use config::{PeerRelayConfig, RelayConfig, WorkspaceBinding};
pub use daemon::{Relay, RelayError, RelayHandle};
pub use home_peer::{HomePeerRegistration, valid_home_peer_label};
pub use mesh::{
    DeviceMeshProfile, DeviceRole, Placement, PlacementRequest, RemoteControlRoute,
    ReplicationIntent, ReplicationResource, ServerAdvertisement, ServiceCapabilities, Workload,
    place, route_remote_control,
};
pub use policy::TrustPolicy;
pub use process_lease::ProcessLease;
pub use transport::ContentPeerRegistration;
pub use transport::{ALPN, Envelope};

/// Semantic version emitted by `qos-relay --version`. Tied to the
/// crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Derive the stable numeric host/gossip shard for a room UUID. This is shared
/// by the shell and admin CLI so local workspace handles never leak onto the
/// wire and operators cannot accidentally bind a different shard.
///
/// # Errors
///
/// Returns an error if `room` is not a valid UUID string.
pub fn room_workspace_id(room: &str) -> Result<u64, uuid::Error> {
    let room = uuid::Uuid::parse_str(room)?;
    let hash = blake3::derive_key("qos-room-transport-v1", room.to_string().as_bytes());
    let mut first8 = [0u8; 8];
    first8.copy_from_slice(&hash[..8]);
    let id = u64::from_le_bytes(first8);
    Ok(if id == 0 { 1 } else { id })
}

/// Stable private relay shard for same-identity continuation checkpoints.
/// This is not a user-visible workspace and is derived only from the root
/// identity so every enrolled device/home peer agrees after restart.
pub fn personal_continuity_workspace_id(user_public_key: &[u8; 32]) -> u64 {
    let hash = blake3::derive_key("qos-personal-continuity-v1\0", user_public_key);
    let mut first8 = [0u8; 8];
    first8.copy_from_slice(&hash[..8]);
    let id = u64::from_le_bytes(first8);
    if id == 0 { 1 } else { id }
}
