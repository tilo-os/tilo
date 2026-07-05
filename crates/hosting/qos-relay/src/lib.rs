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
//! # Non-goals
//!
//! - No compute offload — the relay does not execute app code, it
//!   only forwards + persists CRDT deltas.
//! - No content byte transfer — content hydration continues to
//!   travel over `iroh-blobs` inside the `fs/` crate; the relay
//!   handles only the CRDT-directory tier.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]

pub mod auth;
pub mod client;
pub mod config;
pub mod daemon;
pub mod metrics;
pub mod peer;
pub mod policy;
pub mod store;
pub mod transport;
pub mod workspace;

pub use auth::PeerRole;
pub use client::{ClientError, RelayClient};
pub use config::{PeerRelayConfig, RelayConfig, WorkspaceBinding};
pub use daemon::{Relay, RelayError, RelayHandle};
pub use policy::TrustPolicy;
pub use transport::{ALPN, Envelope};

/// Semantic version emitted by `qos-relay --version`. Tied to the
/// crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
