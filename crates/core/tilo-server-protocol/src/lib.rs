//! `tilo-server-protocol` — the value types shared between the Tilo host +
//! server daemon (`tilo-server`) and the clients that configure, observe or
//! route through it.
//!
//! Everything here is plain data plus the pure functions defined over it:
//!
//! - [`auth`] — the mutual challenge/response handshake messages and their
//!   signing / verification;
//! - [`transport`] — the link ALPNs and the server's own operation messages;
//! - [`live`] — the ephemeral presence and preview frames of the live session;
//! - [`web`] — the HTTPS routes a page asks a host before it holds a session,
//!   and the live grant;
//! - [`workspaces`] — hosted workspaces over HTTPS: administration and joining;
//! - [`directory`] — the public workspaces a host lists, and the entry that
//!   lets anyone in with the owner's default role;
//! - [`operator`] — the wait-list mailbox and status an entry keeps, and the
//!   JSON-lines protocol a server that ran each workspace as a child process
//!   spoke with it before the daemon served them itself;
//! - [`mesh`] — the services a server says it hosts;
//! - [`config`] and [`policy`] — the on-disk `server.toml` schema;
//! - [`hosted`] — what a host hands the browser session in a workspace's VM when
//!   it gives it the workspace, and the address such a machine reaches it at;
//! - [`metrics`] — the counter snapshot the daemon persists for status UIs.
//!
//! The serialized forms are persisted on disk and exchanged on the wire, so
//! they are compatibility surfaces: `tests/formats.rs` pins an encoded
//! example of each.

#![deny(rust_2018_idioms)]

pub mod auth;
pub mod config;
/// Adding a computer to the person's devices with a one-use ticket: the computer's own daemon
/// issues it, prints the one string, and the new computer logs in with it.
pub mod device_tickets;
pub mod devices;
pub mod directory;
pub mod entering;
pub mod hosted;
pub mod hosted_vm;
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
pub use mesh::{DeviceKey, ServiceCapabilities};
pub use metrics::MetricsSnapshot;
pub use policy::TrustPolicy;
pub use transport::DocumentAnswer;
pub use work::{Whose, WorkFrom, WorkKinds, WorkSetting, WorkWhen};
