//! The workspace service every device runs and the server embeds: the governance of
//! the workspaces placed on a device (`workspaces`), the device link their members
//! reach them over (the handshake, `workspace_access`, each workspace's runtime) and
//! the device store the workspace's changes are kept in. [`service::WorkspaceService`]
//! is its embeddable surface; the modules marked hidden are shared with the server
//! daemon that embeds the same stack, and are not an interface of their own.
//!
//! A device's link, its files and its processes are a native target's: a page
//! reaches its host and holds its workspaces over the page's own storage. Those
//! modules are compiled for native targets alone, and a page build names only
//! the document vocabulary ([`workspace_documents`]).
#![deny(rust_2018_idioms)]
#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::future_not_send,
        reason = "a page reaches JavaScript handles bound to its one page thread, and its futures stay on it"
    )
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(not(target_arch = "wasm32"))]
pub mod app_serving;
/// The apps this device serves, on the devices that have any.
#[cfg(all(feature = "apps", not(target_arch = "wasm32")))]
pub mod apps;
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod authority_epoch;
/// How a device opens its own workspace service and the key service beside it.
#[cfg(all(feature = "device-keys", not(target_arch = "wasm32")))]
pub mod own_device;
// The services a person connected by their address: a server calls out
// for them, which a page cannot.
/// The agents a device runs for the workspaces it holds, and the events and look-rounds
/// their turns reach.
#[cfg(all(feature = "agents", not(target_arch = "wasm32")))]
pub mod agents;
#[cfg(not(target_arch = "wasm32"))]
mod connections;
/// Content a record references, reaching the devices that may read it.
#[cfg(all(feature = "content", not(target_arch = "wasm32")))]
pub mod content;
/// A device's own documents, as its workspace service holds them.
#[cfg(not(target_arch = "wasm32"))]
pub mod device_documents;
#[cfg(not(target_arch = "wasm32"))]
pub mod device_tickets;
/// Holding one workspace's documents: what a device reads of them and the writes it authors,
/// over the replica and keys whichever holder keeps them.
pub mod documents;
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod environment;
/// The tokens a host's fetch proxy takes, minted when a page asks its host
/// for one; the proxy admits the person each names.
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod fetch_grants;
/// A workspace's file tree, held in the device's own service.
#[cfg(all(feature = "files", not(target_arch = "wasm32")))]
pub mod files;
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod handshake;
#[cfg(not(target_arch = "wasm32"))]
pub mod history_move;
#[cfg(not(target_arch = "wasm32"))]
mod i18n;
#[cfg(not(target_arch = "wasm32"))]
pub mod metrics;
#[cfg(not(target_arch = "wasm32"))]
mod own_workspaces;
#[cfg(not(target_arch = "wasm32"))]
pub mod payloads;
#[cfg(not(target_arch = "wasm32"))]
pub mod registry;
#[cfg(not(target_arch = "wasm32"))]
pub mod service;
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod service_authority;
#[cfg(not(target_arch = "wasm32"))]
pub mod surfaces;
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod timing;
#[cfg(all(feature = "agents", not(target_arch = "wasm32")))]
pub mod tl_records;
#[cfg(not(target_arch = "wasm32"))]
pub mod work;
#[cfg(not(target_arch = "wasm32"))]
pub mod workspace;
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod workspace_access;
pub mod workspace_documents;
#[cfg(not(target_arch = "wasm32"))]
pub mod workspace_files;
#[cfg(not(target_arch = "wasm32"))]
#[doc(hidden)]
pub mod workspace_io;
#[cfg(not(target_arch = "wasm32"))]
pub mod workspaces;
/// Whether the endpoint a server serves its workspaces on names where it is
/// reached, which says whether opening it waits for that name.
#[cfg(not(target_arch = "wasm32"))]
pub use tilo_endpoint_reachability::Reaching;

#[cfg(all(test, not(target_arch = "wasm32")))]
tilo_automerge_isolate::test_worker!();
#[cfg(all(test, feature = "agents", not(target_arch = "wasm32")))]
tilo_agent_session::app_worker!();

#[cfg(not(target_arch = "wasm32"))]
use tilo_server_client::frame;
#[cfg(not(target_arch = "wasm32"))]
use tilo_server_protocol::{auth, policy};
#[cfg(not(target_arch = "wasm32"))]
mod transport {
    pub(crate) use tilo_server_protocol::transport::*;
}
