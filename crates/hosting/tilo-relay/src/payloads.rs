//! What the host may know about an opaque co-edit body.
//!
//! A [`CoeditPayload`](crate::coedit::CoeditPayload) carries its channel and
//! retention kind in the clear and the collaboration message as an opaque
//! body. The host decisions that need to know a little about that body ask
//! a [`ShellPayloads`] adapter, defined with the desktop's adapter in
//! `tilo-relay-shell`; the transport core never names the desktop's message
//! vocabulary itself.
//!
//! The adapter is injected, not looked up: [`Relay`](crate::daemon::Relay)
//! takes one at start ([`Relay::start`](crate::daemon::Relay::start) wires
//! [`NeighbourPayloads`]) and hands it to the compactor and to every
//! workspace store it opens, and the peer session asks the store of the
//! workspace it serves. The same adapter binds native shell replay
//! ([`ShellPayloads::replay`]): the store steps the selection the bound
//! authority hands it through the `store::shell_selection` seam. Room
//! governance still uses the desktop's vocabulary directly.
//! An adapter that classifies nothing makes the host keep every entry and
//! fold none: unknown data never grants removal.

use std::sync::Arc;

pub use tilo_relay_shell::{Checkpoint, CheckpointKind, NeighbourPayloads, Route, ShellPayloads};

/// The desktop's adapter, as [`Relay::start`](crate::daemon::Relay::start)
/// and [`WorkspaceStore::open`](crate::store::WorkspaceStore::open) wire it.
#[must_use]
pub fn neighbour() -> Arc<dyn ShellPayloads> {
    Arc::new(NeighbourPayloads)
}
