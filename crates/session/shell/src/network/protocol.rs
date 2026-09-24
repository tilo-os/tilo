//! Peer-sync wire protocol.
//!
//! The vocabulary lives in [`neighbour_wire`] so any peer that speaks it —
//! not only the compositor — shares one definition and one pinned encoding.
//! Re-exported here so compositor call sites keep their paths.

pub use neighbour_wire::{
    MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES, NeighbourMsg, PeerId, PresenceKey, SurfaceId,
};
