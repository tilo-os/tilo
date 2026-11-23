//! `tilo-server-protocol` — the value types shared between the Tilo host +
//! server daemon (`tilo-server`) and the clients that configure, observe or
//! route through it.
//!
//! Everything here is plain data plus the pure functions defined over it:
//!
//! - [`auth`] — the mutual challenge/response handshake messages and their
//!   signing / verification;
//! - [`transport`] — the server ALPNs and the post-handshake [`Envelope`]
//!   plus the discovery and home-peer management messages;
//! - [`certify`] — server-mediated issuance of a `DeviceCert` to a device
//!   that holds only its own key;
//! - [`compaction`] — a member device's coverage statement for a sealed
//!   route the host cannot read;
//! - [`live`] — the ephemeral presence and preview frames of the live session;
//! - [`web`] — the HTTPS routes a page asks a host before it holds a session,
//!   and the live grant;
//! - [`workspaces`] — governed workspaces over HTTPS: administration and joining;
//! - [`directory`] — the public workspaces a host lists, and the entry that
//!   lets anyone in with the owner's default role;
//! - [`operator`] — the JSON-lines protocol between the public-workspace
//!   service and each `tilo-server workspace` child it runs;
//! - [`mesh`] — service capabilities and the server advertisements devices
//!   offer work with;
//! - [`config`] and [`policy`] — the on-disk `server.toml` schema;
//! - [`home_peer`] — the registry record written after enrolling an
//!   always-on peer;
//! - [`hosted`] — what a host hands the browser session in a workspace's VM when
//!   it gives it the workspace, and the address such a machine reaches it at;
//! - [`metrics`] — the counter snapshot the daemon persists for status UIs.
//!
//! The serialized forms are persisted on disk and exchanged on the wire, so
//! they are compatibility surfaces: `tests/formats.rs` pins an encoded
//! example of each.

#![deny(rust_2018_idioms)]

pub mod agents;
pub mod auth;
pub mod certify;
pub mod compaction;
pub mod config;
pub mod devices;
pub mod directory;
mod earlier;
pub mod entering;
pub mod host_voice;
pub mod hosted;
pub mod invocation;
pub mod live;
pub mod mesh;
pub mod metrics;
pub mod operator;
pub mod page;
pub mod policy;
pub mod public_avatars;
pub mod refusal;
pub mod servers;
pub mod transport;
pub mod web;
pub mod work;
pub mod workspaces;

/// A workspace's id, defined beside [`delegation`].
pub use tilo_workspace_join::WorkspaceId;
/// Root-signed agent and bot delegations, defined in `tilo-workspace-join` so
/// the admission core can verify them without linking the server protocol.
pub use tilo_workspace_join::delegation;

pub use auth::{AuthError, AuthenticatedPeer};
pub use config::{ConfigError, ServerConfig};
pub use devices::valid_device_label;
pub use mesh::{
    ComputeAccelerator, DeviceKey, DeviceMeshProfile, PowerAvailability, ServerAdvertisement,
    ServiceCapabilities,
};
pub use metrics::MetricsSnapshot;
pub use policy::TrustPolicy;
pub use transport::{ALPN, DocumentAnswer, Envelope};
pub use work::{Whose, WorkFrom, WorkKinds, WorkSetting, WorkWhen};

/// Domain-separated secret the server binds, derived from the device secret.
///
/// The server and the filesystem must not publish competing address records
/// for one Iroh node id. Callers that only need the dial target use
/// [`server_node_id`].
#[must_use]
pub fn server_node_secret(device_secret: &[u8; 32]) -> [u8; 32] {
    blake3::derive_key("tilo-server-iroh-service-key-v1", device_secret)
}

/// Public Iroh endpoint id of the server that belongs to this device secret.
///
/// This is the value stored as `server_node_id`. It is not the device
/// public key, and deriving it does not require sending the device secret.
#[must_use]
pub fn server_node_id(device_secret: &[u8; 32]) -> [u8; 32] {
    iroh_base::SecretKey::from_bytes(&server_node_secret(device_secret))
        .public()
        .as_bytes()
        .to_owned()
}
