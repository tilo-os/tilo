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
//! - [`environment`] — interactive environment sessions a relay runs for its
//!   members, and the legacy launches they route to KVM workers;
//! - [`legacy_window`] — legacy windows a session streams, as the live plane
//!   carries them to browsers, and the input members send back;
//! - [`web`] — the browser carriers: HTTPS routes and the live grant;
//! - [`rooms`] — governed rooms over HTTPS: administration and joining;
//! - [`room_operator`] — the JSON-lines protocol between the public-room
//!   service and each `tilo-relay room` child it runs;
//! - [`mesh`] — device roles, service capabilities, replication intents and
//!   the server advertisements compute admission checks;
//! - [`placement`] — which advertised device a compute job is sent to, and
//!   why every other one was passed over;
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

pub mod agents;
pub mod auth;
pub mod certify;
pub mod compaction;
pub mod config;
pub mod environment;
pub mod home_peer;
pub mod legacy_window;
pub mod live;
pub mod mesh;
pub mod metrics;
pub mod placement;
pub mod policy;
pub mod process_lease;
pub mod public_avatars;
pub mod refusal;
pub mod retention;
pub mod room_operator;
pub mod rooms;
pub mod transport;
pub mod voice;
pub mod web;

/// Root-signed agent and bot delegations, defined in `tilo-workspace-join` so
/// the admission core can verify them without linking the relay protocol.
pub use tilo_workspace_join::delegation;
/// The stable relay shard of a room UUID, defined beside [`delegation`].
pub use tilo_workspace_join::room_workspace_id;

pub use auth::{AuthError, AuthenticatedPeer, PeerRole};
pub use config::{ConfigError, EnvironmentsConfig, PeerRelayConfig, RelayConfig, WorkspaceBinding};
pub use home_peer::{
    HomePeerRegistration, RELAY_OFFERS_FILE, RelayEnvironments, RelayOffer, RelayOffers,
    home_peer_registry_path, personal_device_peer_path, remember_personal_device,
    valid_home_peer_label,
};
pub use mesh::{
    ComputeAccelerator, DeviceKey, DeviceMeshProfile, DeviceRole, PowerAvailability,
    ReplicationIntent, ReplicationResource, ServerAdvertisement, ServiceCapabilities,
};
pub use metrics::MetricsSnapshot;
pub use placement::{
    ComputeRequirements, Ineligible, NoEligibleWorker, Placement, PlacementCandidate,
    PlacementRank, PowerClass, place_compute_job,
};
pub use policy::TrustPolicy;
pub use process_lease::ProcessLease;
pub use transport::{ALPN, ContentPeerRegistration, Envelope};

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

/// Domain-separated secret the relay binds, derived from the device secret.
///
/// The relay and the filesystem must not publish competing address records
/// for one Iroh node id. Callers that only need the dial target use
/// [`relay_service_node_id`].
#[must_use]
pub fn relay_service_secret(device_secret: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key("tilo-relay-iroh-service-key-v1", device_secret)
}

/// Public Iroh endpoint id of the relay that belongs to this device secret.
///
/// This is the value stored as `relay_service_node_id`. It is not the device
/// public key, and deriving it does not require sending the device secret.
#[must_use]
pub fn relay_service_node_id(device_secret: &[u8; 32]) -> [u8; 32] {
    iroh_base::SecretKey::from_bytes(&relay_service_secret(device_secret))
        .public()
        .as_bytes()
        .to_owned()
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
