//! `tilo-relay-session` — the client side of a Tilo relay session.
//!
//! A device that talks to a relay needs the authenticated session client
//! ([`RelayClient`]), the co-edit session over it ([`RelayCoeditSession`]),
//! anti-entropy ([`antientropy`]), resume positions ([`position`]), room
//! administration ([`rooms::RoomAdminClient`]) and the workspace join client
//! ([`workspace_join::WorkspaceJoinClient`]). None of them needs the daemon's
//! store, delivery or join records, so they live here and the daemon
//! (`tilo-relay`) depends on this crate rather than the other way round.
//! Clients link this crate alone and are not rebuilt by a daemon edit.
//!
//! The wire messages live in `tilo-relay-protocol` and the frame codec in
//! `tilo-relay-client`.

#![deny(rust_2018_idioms)]
#![warn(missing_docs)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod antientropy;
pub mod client;
pub mod coedit;
pub mod position;
pub mod rooms;
mod session_carrier;
pub mod workspace_join;

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
tilo_automerge_isolate::test_worker!();

// The session modules address the protocol crate under the daemon's historic
// paths (`crate::auth`, `crate::transport`, …); keep those crate-private.
#[cfg(test)]
use tilo_relay_protocol::room_workspace_id;
use tilo_relay_protocol::{auth, live, room_workspace_id_matches};

/// The relay wire messages together with the frame error, under the path the
/// session modules use for both.
mod transport {
    pub use tilo_relay_client::frame::FrameError;
    pub use tilo_relay_protocol::transport::*;
}

pub use antientropy::{SyncError, apply_sync_response, heads_wire, reconcile, send_sync_request};
pub use client::{ClientError, RelayClient};
pub use coedit::{AttributedPresence, CoeditError, CoeditPayload, Inbound, RelayCoeditSession};

/// Whether low-volume, payload-free event correlation is logged. The same
/// switch as the daemon's, so one variable correlates both ends.
pub(crate) fn correlated_diagnostics() -> bool {
    std::env::var_os("TILO_RELAY_CORRELATED_DIAGNOSTICS").is_some()
}

/// Trace one hop of a native publication under the `tilo_relay::native_publication`
/// target. Hashes the same signed message bytes as the native publication
/// cursor; the target is opt-in, so ordinary operation does not decode or hash.
pub fn trace_native_publication(
    phase: &str,
    envelope: &transport::Envelope,
    recipient: Option<&[u8; 32]>,
) {
    if !tracing::enabled!(target: "tilo_relay::native_publication", tracing::Level::DEBUG) {
        return;
    }
    let transport::Envelope::Delta {
        workspace_id,
        author_device_public_key,
        delta,
        ..
    } = envelope
    else {
        return;
    };
    let Some(payload) = crate::coedit::CoeditPayload::decode(delta) else {
        return;
    };
    let (kind, body) = match payload {
        crate::coedit::CoeditPayload::Checkpoint { body, .. } => ("checkpoint", body),
        crate::coedit::CoeditPayload::RetainedDelta { body, .. } => ("delta", body),
    };
    tracing::debug!(target: "tilo_relay::native_publication",
        phase, id = %blake3::hash(&body), kind, workspace_id,
        author = %hex::encode(&author_device_public_key[..4]),
        recipient = ?recipient.map(|key| hex::encode(&key[..4])),
        "native relay publication hop");
}
