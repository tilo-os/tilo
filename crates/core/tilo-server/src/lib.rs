//! `tilo-server` — the Tilo host + server daemon.
//!
//! # Model
//!
//! A **host** in Tilo is a device that (a) presents a `DeviceCert` chaining to
//! your user identity, (b) is marked always-on, and (c) serves the workspaces
//! placed on it: their records, governance and channels, and the actions their
//! devices ask. The same daemon also **keeps** a workspace's history while a
//! device is away, and hands it to that device when it links again.
//!
//! # Wire model
//!
//! The daemon binds a single iroh endpoint, and opens a WebTransport listener
//! for pages. A device proves itself in the authenticated handshake
//! ([`Hello`](tilo_server_protocol::auth::Hello)): it sends its `DeviceCert`
//! and a challenge nonce, the host answers with its own certificate, a nonce
//! and a signature over the client's nonce, and the device signs the host's
//! nonce in return. Both sides have then proved a device key chaining to an
//! identity the host admits.
//!
//! What follows the proof is the device's link to a workspace
//! ([`tilo_peer_state::direct`]): the workspace's records, governance and
//! channels travel it, and the host answers the device's writes there. The
//! actions a device asks travel the same way
//! ([`tilo_peer_state::actions`]), and a request addressed to another device
//! of the workspace is passed on over the link that reaches it.
//!
//! # Multi-host
//!
//! Two hosts serving the same workspace are holders of the workspace's
//! history, alike from its governance's point of view. Which device holds a
//! workspace, and in which role, is the workspace's own governance
//! ([`tilo_peer_state::WORKSPACE_KEEPING_DEVICES_KEY`]), so every member learns
//! each holder as it learns any change, and reaches the workspace wherever it
//! is held.
#![deny(rust_2018_idioms)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(feature = "agents")]
pub mod apple_push;
#[cfg(feature = "agents")]
pub mod browser_actions;
#[cfg(not(target_arch = "wasm32"))]
pub mod certification;
#[cfg(not(target_arch = "wasm32"))]
pub mod content;
#[cfg(feature = "web")]
pub mod content_links;
#[cfg(not(target_arch = "wasm32"))]
pub mod daemon;
#[cfg(feature = "web")]
pub mod entry;
#[cfg(feature = "agents")]
pub mod hosted_browser;
#[cfg(not(target_arch = "wasm32"))]
pub mod kept_content;
#[cfg(feature = "agents")]
pub mod linux_devices;
#[cfg(feature = "agents")]
pub mod machine_agents;
#[cfg(not(target_arch = "wasm32"))]
pub mod servers;
#[cfg(feature = "web")]
pub mod web;
#[cfg(feature = "web")]
pub mod web_push;
pub mod workspace_join;

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
tilo_automerge_isolate::test_worker!();
#[cfg(all(test, feature = "agents"))]
tilo_agent_session::app_worker!();

// The records and wire messages shared with server clients live in
// `tilo-server-protocol`, the frame codec in `tilo-server-client`, and the
// workspace administration vocabulary in `tilo-workspace-service`, so clients
// need not link the daemon. The daemon keeps addressing them here.
#[cfg(not(target_arch = "wasm32"))]
pub use tilo_workspace_service::{
    Reaching, app_serving, device_documents, environment, metrics, payloads, registry, service,
    surfaces, workspace, workspace_documents, workspace_files, workspaces,
};
#[cfg(not(target_arch = "wasm32"))]
use tilo_workspace_service::{authority_epoch, handshake, workspace_access};

pub use tilo_server_client::frame;
pub use tilo_server_protocol::{auth, certify, config, devices, live, mesh, policy};

#[cfg(not(target_arch = "wasm32"))]
pub use app_serving::{ActionProviderServer, AppActionServer};
pub use config::ServerConfig;
#[cfg(not(target_arch = "wasm32"))]
pub use daemon::{Server, ServerError, ServerHandle};
pub use devices::valid_device_label;
pub use mesh::{
    ComputeAccelerator, DeviceMeshProfile, PowerAvailability, ServerAdvertisement,
    ServiceCapabilities,
};
pub use policy::TrustPolicy;

/// Semantic version emitted by `tilo-server --version`. Tied to the
/// crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use tilo_server_protocol::WorkspaceId;

#[cfg(not(target_arch = "wasm32"))]
pub mod operations;
pub mod rollout;
