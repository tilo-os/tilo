//! The workspace host every device runs and the server embeds: the governance of
//! the workspaces placed on a device (`workspaces`), the session stack their members
//! run (the handshake, `peer`, each workspace's runtime) and the retained
//! store and persistence beneath it. [`host::WorkspaceHost`] is its embeddable
//! surface; the modules marked hidden are shared with the server daemon that
//! embeds the same stack, and are not an interface of their own.
#![deny(rust_2018_idioms)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod app_serving;
pub mod attach;
#[doc(hidden)]
pub mod authority_epoch;
pub mod compaction;
// The services a person connected by their address: a server calls out
// for them, which a page cannot.
#[cfg(not(target_arch = "wasm32"))]
mod connections;
pub mod content_queue;
pub mod content_work;
#[doc(hidden)]
pub mod environment;
#[doc(hidden)]
pub mod handshake;
pub mod host;
#[doc(hidden)]
pub mod host_authority;
mod hosted;
mod i18n;
pub mod member_limits;
#[doc(hidden)]
pub mod membership_watch;
pub mod metrics;
mod own_workspaces;
pub mod payloads;
pub mod peer;
#[doc(hidden)]
pub mod persistence;
pub mod presence_mailbox;
pub mod store;
pub mod surfaces;
#[doc(hidden)]
pub mod timing;
pub mod workspace;
#[doc(hidden)]
pub mod workspace_access;
pub mod workspace_documents;
#[doc(hidden)]
pub mod workspace_io;
pub mod workspaces;

#[cfg(test)]
tilo_automerge_isolate::test_worker!();
#[cfg(all(test, feature = "agents"))]
tilo_agent_session::app_worker!();

use tilo_server_client::frame;
use tilo_server_protocol::{auth, policy};
use tilo_server_session::{client, coedit, position};

mod transport {
    pub(crate) use crate::frame::{decode, encode};
    pub(crate) use tilo_server_protocol::transport::*;
}

#[cfg(all(test, feature = "agents"))]
use client::ServerClient;
use coedit::AttributedPresence;
#[cfg(all(test, feature = "agents"))]
use coedit::CoeditPayload;

#[cfg(all(test, feature = "agents"))]
use transport::Envelope;
