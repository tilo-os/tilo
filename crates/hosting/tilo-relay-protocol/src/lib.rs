//! `tilo-relay-protocol` — the value types shared between the Tilo host +
//! relay daemon (`tilo-relay`) and the clients that configure, observe or
//! route through it.
//!
//! Everything here is plain data plus the pure functions defined over it:
//!
//! - [`auth`] — the mutual challenge/response handshake messages and their
//!   signing / verification;
//! - [`transport`] — the relay ALPNs and the post-handshake [`Envelope`]
//!   plus the discovery and home-peer management messages;
//! - [`mesh`] — device roles, service capabilities, replication intents and
//!   the deterministic placement / remote-control routing over them;
//! - [`config`] and [`policy`] — the on-disk `relay.toml` schema;
//! - [`home_peer`] — the registry record written after enrolling an
//!   always-on peer;
//! - [`metrics`] — the counter snapshot the daemon persists for status UIs;
//! - [`process_lease`] — the PID + start-time pair carried across the
//!   enrollment privilege boundary;
//! - [`room_workspace_id`] / [`personal_continuity_workspace_id`] — the
//!   stable relay shard ids every peer must derive identically.
//!
//! The serialized forms are persisted on disk and exchanged on the wire, so
//! they are compatibility surfaces: `tests/formats.rs` pins an encoded
//! example of each.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod auth;
pub mod config;
pub mod home_peer;
pub mod mesh;
pub mod metrics;
pub mod policy;
pub mod process_lease;
mod sig64;
pub mod transport;

pub use auth::{AuthError, AuthenticatedPeer, PeerRole};
pub use config::{ConfigError, PeerRelayConfig, RelayConfig, WorkspaceBinding};
pub use home_peer::{HomePeerRegistration, valid_home_peer_label};
pub use mesh::{
    ComputeAccelerator, DeviceKey, DeviceMeshProfile, DeviceRole, Placement, PlacementRequest,
    PowerAvailability, RemoteControlRoute, ReplicationIntent, ReplicationResource,
    ServerAdvertisement, ServiceCapabilities, Workload, place, route_remote_control,
};
pub use metrics::MetricsSnapshot;
pub use policy::TrustPolicy;
pub use process_lease::ProcessLease;
pub use transport::{ALPN, ContentPeerRegistration, Envelope};

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
