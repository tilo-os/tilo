//! `tilo-server` — the Tilo host + server daemon.
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
//! connection speaks a Tilo server ALPN and performs a small
//! authenticated handshake ([`Hello`](tilo_server_protocol::auth::Hello)):
//!
//! 1. Client sends its `DeviceCert` + a challenge nonce.
//! 2. Server verifies the cert chains to *this server's* user
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
//! - App backends are not executed here.
//! - No content byte transfer — content hydration continues to
//!   travel over `iroh-blobs` inside the `crates/system/fs/` crate; the relay
//!   handles only the CRDT-directory tier.

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
pub mod agents;
#[cfg(feature = "agents")]
pub mod apple_push;
#[cfg(feature = "agents")]
pub mod browser_actions;
#[cfg(not(target_arch = "wasm32"))]
pub mod certification;
#[cfg(feature = "agents")]
pub mod chat_attachments;
#[cfg(not(target_arch = "wasm32"))]
pub mod content;
#[cfg(feature = "web")]
pub mod content_links;
#[cfg(not(target_arch = "wasm32"))]
pub mod daemon;
#[cfg(feature = "web")]
pub mod entry;
#[cfg(feature = "agents")]
pub mod event_runner;
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
#[cfg(not(target_arch = "wasm32"))]
pub mod state_lock;
#[cfg(feature = "web")]
pub mod web;
#[cfg(feature = "web")]
pub mod web_push;
pub mod workspace_join;
mod workspace_keys;

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
tilo_automerge_isolate::test_worker!();
#[cfg(all(test, feature = "agents"))]
tilo_agent_session::app_worker!();

// The records and wire messages shared with server clients live in
// `tilo-server-protocol`, the frame codec in `tilo-server-client`, and the
// session clients (server, co-edit and workspace administration, and
// workspace join) in `tilo-server-session`, so clients need not link the
// daemon. The daemon keeps addressing them here.
#[cfg(not(target_arch = "wasm32"))]
pub use tilo_workspace_service::{
    Reaching, app_serving, device_documents, environment, metrics, payloads, peer,
    presence_mailbox, service, surfaces, workspace, workspace_documents, workspace_files,
    workspaces,
};
#[cfg(not(target_arch = "wasm32"))]
use tilo_workspace_service::{authority_epoch, handshake, workspace_access};

pub use tilo_server_client::frame;
pub use tilo_server_protocol::{auth, certify, config, devices, live, mesh, policy};
pub use tilo_server_session::{client, coedit};

/// The server wire messages ([`tilo_server_protocol::transport`]) together with
/// the envelope frame codec ([`frame`]), under the path the daemon and its
/// callers have always used for both.
pub mod transport {
    pub use crate::frame::{FrameError, FrameReader, decode, encode};
    pub use tilo_server_protocol::transport::*;
}

#[cfg(not(target_arch = "wasm32"))]
pub use app_serving::{ActionProviderServer, AppActionServer};
pub use client::{ClientError, ServerClient};
pub use coedit::{AttributedPresence, CoeditError, CoeditPayload, Inbound, ServerCoeditSession};
pub use config::ServerConfig;
#[cfg(not(target_arch = "wasm32"))]
pub use daemon::{Server, ServerError, ServerHandle};
pub use devices::valid_device_label;
pub use mesh::{
    ComputeAccelerator, DeviceMeshProfile, PowerAvailability, ServerAdvertisement,
    ServiceCapabilities,
};
pub use policy::TrustPolicy;
pub use transport::{ALPN, Envelope};

/// Semantic version emitted by `tilo-server --version`. Tied to the
/// crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub use tilo_server_protocol::WorkspaceId;

#[cfg(not(target_arch = "wasm32"))]
pub mod operations;
pub mod rollout;
#[cfg(not(target_arch = "wasm32"))]
pub mod root_signer;
