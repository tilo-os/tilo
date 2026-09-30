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
//! - [`certify`] — relay-mediated issuance of a `DeviceCert` to a device
//!   that holds only its own key;
//! - [`compaction`] — a member device's coverage statement for a sealed
//!   route the host cannot read;
//! - [`live`] — the ephemeral presence and preview frames of the live plane;
//! - [`web`] — the browser carriers: HTTPS routes and the live grant;
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

pub mod agents;
pub mod auth;
pub mod certify;
pub mod compaction;
pub mod config;
pub mod home_peer;
pub mod live;
pub mod mesh;
pub mod metrics;
pub mod policy;
pub mod process_lease;
pub mod retention;
mod sig64;
pub mod transport;
pub mod voice;
pub mod web;

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
    // Encode on the stack: bounded delivery paths derive this id under a
    // native allowance that funds no heap allocation.
    let mut canonical = [0u8; 36];
    let hash = blake3::derive_key(
        "tilo-room-transport-v1",
        room.hyphenated().encode_lower(&mut canonical).as_bytes(),
    );
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

/// Match a retained or subscribed binding without aliasing arbitrary numeric IDs.
pub fn room_workspace_id_matches(room: &str, workspace: u64) -> bool {
    room_workspace_id(room).is_ok_and(|id| id == workspace)
}

/// Only the shard derived from this authenticated identity is private.
pub fn personal_continuity_workspace_id_matches(
    user_public_key: &[u8; 32],
    workspace: u64,
) -> bool {
    workspace == personal_continuity_workspace_id(user_public_key)
}

#[cfg(test)]
mod binding_tests {
    #[test]
    fn bindings_are_scoped_to_exact_room_and_root_identity() {
        let room = "11111111-1111-4111-8111-111111111111";
        let other = "22222222-2222-4222-8222-222222222222";
        let id = super::room_workspace_id(room).unwrap();
        assert!(super::room_workspace_id_matches(room, id));
        assert!(!super::room_workspace_id_matches(other, id));
        assert!(!super::room_workspace_id_matches("invalid", id));
        let id = super::personal_continuity_workspace_id(&[7; 32]);
        assert!(super::personal_continuity_workspace_id_matches(
            &[7; 32], id
        ));
        assert!(!super::personal_continuity_workspace_id_matches(
            &[8; 32], id
        ));
    }
}
