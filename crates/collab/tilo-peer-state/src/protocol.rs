//! Peer-sync wire protocol. The vocabulary is owned by `neighbour-wire`, the
//! one crate native and portable hosts share, so a message encoded by either
//! side decodes as the same type on the other. Re-exported here for the
//! existing `tilo_peer_state::protocol` callers.

pub use neighbour_wire::{
    MAX_PERSONAL_CONTINUITY_CHECKPOINT_BYTES, NeighbourMsg, PeerId, PresenceKey, PresenceSlot,
    SurfaceId,
};
