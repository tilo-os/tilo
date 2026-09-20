//! Shared native/browser neighbour wire and certified state admission.
pub mod colorimetry;
pub mod hdr_metadata;
pub mod protocol;
pub use protocol::NeighbourMsg;
#[cfg(test)]
mod admission_tests;
mod cached_document;
pub mod document;
pub mod room;
mod state;
mod verified_certificate;
pub use state::{
    DocumentRow, ReceiveOutcome, SeedDocument, SeedState, StatePeer, TransportOrigin, peer_handle,
    seed_room,
};
pub mod coverage;
pub mod transport;
pub use collab_types::DocumentMutation;

pub mod outbox;

#[cfg(test)]
mod enrollment_tests;

pub mod enrollment;

pub mod durability;

pub mod topology;

pub mod desktop_command;
pub mod owner_commit;

#[cfg(not(target_arch = "wasm32"))]
pub mod retained_proof;

pub mod desktop_delivery;
