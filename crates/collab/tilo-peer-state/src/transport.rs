//! Host retention, re-exported from its one owner, [`neighbour_wire::retention`].
//!
//! Native publication, the relay and browser peers must classify a message
//! identically, or a host's retained history would depend on which client
//! wrote it. This module keeps existing `tilo_peer_state::transport` paths
//! compiling; it holds no policy of its own.
pub use neighbour_wire::{Retention, app_document_channel, retention};
