//! The room host every device runs and the relay embeds: the governance of
//! the rooms placed on a device (`rooms`), the session stack their members
//! run (the handshake, `peer`, each workspace's runtime) and the retained
//! store and persistence beneath it. [`host::RoomHost`] is its embeddable
//! surface; the modules marked hidden are shared with the relay daemon that
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
#[doc(hidden)]
pub mod authority_epoch;
pub mod compaction;
mod desktop_guard;
pub mod federation;
#[doc(hidden)]
pub mod handshake;
pub mod host;
#[doc(hidden)]
pub mod host_authority;
mod hosted;
pub mod member_limits;
#[doc(hidden)]
pub mod membership_watch;
pub mod metrics;
pub mod payloads;
pub mod peer;
#[doc(hidden)]
pub mod persistence;
pub mod presence_mailbox;
#[doc(hidden)]
pub mod room_access;
pub mod room_records;
pub mod rooms;
pub mod store;
pub mod surfaces;
#[doc(hidden)]
pub mod timing;
pub mod workspace;
#[doc(hidden)]
pub mod workspace_io;

#[cfg(test)]
tilo_automerge_isolate::test_worker!();
#[cfg(all(test, feature = "agents"))]
tilo_agent_participant::app_worker!();

use tilo_relay_client::frame;
use tilo_relay_protocol::{auth, live, policy};
use tilo_relay_session::{client, coedit, position};

mod transport {
    pub(crate) use crate::frame::{decode, encode};
    pub(crate) use tilo_relay_protocol::transport::*;
}

#[cfg(all(test, feature = "agents"))]
use client::RelayClient;
use coedit::AttributedPresence;
#[cfg(all(test, feature = "agents"))]
use coedit::CoeditPayload;
use tilo_relay_protocol::{room_workspace_id, room_workspace_id_matches};
#[cfg(all(test, feature = "agents"))]
use transport::Envelope;
