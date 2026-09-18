//! Shared native/browser neighbour wire and certified state admission.
pub mod colorimetry;
pub mod hdr_metadata;
pub mod protocol;
pub use protocol::NeighbourMsg;
pub mod room;
pub mod document;
#[cfg(test)] mod admission_tests;
mod state;
pub use state::{StatePeer,SeedState,SeedDocument,DocumentRow,ReceiveOutcome,TransportOrigin,seed_room,peer_handle};
pub mod transport;
pub mod coverage;
pub use collab_types::DocumentMutation;
