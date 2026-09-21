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
//! connection speaks a current Tilo ALPN or an explicit frozen qOS
//! compatibility ALPN and performs a small
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
pub mod clock;
pub mod coedit;
#[cfg(not(target_arch = "wasm32"))]
pub mod compute;
#[cfg(not(target_arch = "wasm32"))]
pub mod compute_session;
#[cfg(not(target_arch = "wasm32"))]
pub mod compute_transport;
#[cfg(not(target_arch = "wasm32"))]
pub mod compute_worker;
#[cfg(not(target_arch = "wasm32"))]
pub mod config;
#[cfg(not(target_arch = "wasm32"))]
pub mod daemon;
pub mod framing;
#[cfg(not(target_arch = "wasm32"))]
pub mod home_peer;
pub mod mesh;
#[cfg(not(target_arch = "wasm32"))]
pub mod metrics;
#[cfg(not(target_arch = "wasm32"))]
pub mod peer;
pub mod peer_actions;
#[cfg(not(target_arch = "wasm32"))]
mod persistence;
pub mod policy;
#[cfg(not(target_arch = "wasm32"))]
pub mod process_lease;
#[cfg(not(target_arch = "wasm32"))]
pub mod store;
pub mod transport;
pub mod transport_v2;
#[cfg(not(target_arch = "wasm32"))]
pub mod workspace;
#[cfg(not(target_arch = "wasm32"))]
mod workspace_io;

pub use antientropy::{SyncError, apply_sync_response, heads_wire, reconcile, send_sync_request};
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
    ComputeWorkerError, FileComputeContentStore, ProductionComputeWorkerConfig,
    run_production_compute_worker,
};
#[cfg(not(target_arch = "wasm32"))]
pub use config::{CheckpointPrefixRecovery, PeerRelayConfig, RelayConfig, WorkspaceBinding};
#[cfg(not(target_arch = "wasm32"))]
pub use daemon::{Relay, RelayError, RelayHandle};
#[cfg(not(target_arch = "wasm32"))]
pub use home_peer::{HomePeerRegistration, valid_home_peer_label};
pub use mesh::{
    ComputeAccelerator, DeviceMeshProfile, DeviceRole, Placement, PlacementRequest,
    PowerAvailability, RemoteControlRoute, ReplicationIntent, ReplicationResource,
    ServerAdvertisement, ServiceCapabilities, Workload, place, route_remote_control,
};
pub use policy::TrustPolicy;
#[cfg(not(target_arch = "wasm32"))]
pub use process_lease::ProcessLease;
pub use transport::ContentPeerRegistration;
pub use transport::{ALPN, Envelope};

/// Semantic version emitted by `tilo-relay --version`. Tied to the
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
    let hash = blake3::derive_key("tilo-room-transport-v1", room.to_string().as_bytes());
    let mut first8 = [0u8; 8];
    first8.copy_from_slice(&hash[..8]);
    let id = u64::from_le_bytes(first8);
    Ok(if id == 0 { 1 } else { id })
}

/// Stable private relay shard for same-identity continuation checkpoints.
/// This is not a user-visible workspace and is derived only from the root
/// identity so every enrolled device/home peer agrees after restart.
pub fn personal_continuity_workspace_id(user_public_key: &[u8; 32]) -> u64 {
    let hash = blake3::derive_key("tilo-personal-continuity-v1\0", user_public_key);
    let mut first8 = [0u8; 8];
    first8.copy_from_slice(&hash[..8]);
    let id = u64::from_le_bytes(first8);
    if id == 0 { 1 } else { id }
}

/// Both frozen room namespaces, derived only from the authenticated room UUID.
/// The first entry is the current Tilo namespace; the second is deployed qOS.
///
/// # Errors
/// Returns an error when the room is not a UUID.
pub fn room_workspace_ids(room: &str) -> Result<[u64; 2], uuid::Error> {
    let room = uuid::Uuid::parse_str(room)?;
    Ok([
        room_workspace_id(&room.to_string())?,
        legacy_room_workspace_id(&room.to_string())?,
    ])
}

/// Preserve the frozen qOS shard for an existing, authenticated room binding.
///
/// # Errors
/// Returns an error when the room is not a UUID.
pub fn legacy_room_workspace_id(room: &str) -> Result<u64, uuid::Error> {
    let room = uuid::Uuid::parse_str(room)?;
    let hash = blake3::derive_key("qos-room-transport-v1", room.to_string().as_bytes());
    let mut first = [0; 8];
    first.copy_from_slice(&hash[..8]);
    Ok(u64::from_le_bytes(first).max(1))
}

/// Match a retained or subscribed binding without aliasing arbitrary numeric IDs.
pub fn room_workspace_id_matches(room: &str, workspace: u64) -> bool {
    room_workspace_ids(room).is_ok_and(|ids| ids.contains(&workspace))
}

/// Frozen legacy private shard; never changes an existing continuity directory.
pub fn legacy_personal_continuity_workspace_id(user_public_key: &[u8; 32]) -> u64 {
    let hash = blake3::derive_key("qos-personal-continuity-v1\0", user_public_key);
    let mut first = [0; 8];
    first.copy_from_slice(&hash[..8]);
    u64::from_le_bytes(first).max(1)
}

/// Only the two namespaces derived from this authenticated identity are private.
pub fn personal_continuity_workspace_id_matches(
    user_public_key: &[u8; 32],
    workspace: u64,
) -> bool {
    workspace == personal_continuity_workspace_id(user_public_key)
        || workspace == legacy_personal_continuity_workspace_id(user_public_key)
}

#[cfg(test)]
mod binding_tests {
    #[test]
    fn aliases_are_scoped_to_exact_room_and_root_identity() {
        let room = "11111111-1111-4111-8111-111111111111";
        let other = "22222222-2222-4222-8222-222222222222";
        let ids = super::room_workspace_ids(room).unwrap();
        assert_ne!(ids[0], ids[1]);
        for id in ids {
            assert!(super::room_workspace_id_matches(room, id));
            assert!(!super::room_workspace_id_matches(other, id));
            assert!(!super::room_workspace_id_matches("invalid", id));
        }
        for id in [
            super::personal_continuity_workspace_id(&[7; 32]),
            super::legacy_personal_continuity_workspace_id(&[7; 32]),
        ] {
            assert!(super::personal_continuity_workspace_id_matches(
                &[7; 32], id
            ));
            assert!(!super::personal_continuity_workspace_id_matches(
                &[8; 32], id
            ));
        }
    }
}
