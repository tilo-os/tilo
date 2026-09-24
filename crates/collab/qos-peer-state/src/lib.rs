//! Shared native/browser neighbour wire and certified state admission.
pub mod colorimetry;
pub mod governance;
pub mod hdr_metadata;
pub mod join;
pub mod protocol;
pub use protocol::NeighbourMsg;
#[cfg(test)]
mod admission_tests;
mod cached_document;
pub mod document;
#[cfg(test)]
mod join_contract_tests;
pub mod room;
mod state;
mod verified_certificate;
pub use state::{
    BoundedPeerError, BoundedPeerInvalid, BoundedStatePeer, DocumentRow, GovernedAccess,
    GovernedMember, LocalRecoveryReservation, NativePublicationCut, PeerDeliveryPolicy,
    PeerDeliveryPolicyError, PeerDocumentRead, PeerDocumentReadError, PeerLimits, PeerMediaCall,
    PeerMember, PeerPolicyBasis, PeerRead, PeerRegistry, PeerRevocations, PeerRoomAccess, PeerTurn,
    PeerWork, ReceiveOutcome, SeedDocument, SeedState, StatePeer, TransportOrigin,
    VerifiedSecurityMetadataOnly, peer_handle, seed_room,
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

mod admission;
#[cfg(test)]
mod native_footprint_vendor_tests;
#[cfg(any(test, doctest))]
mod native_read_vendor_tests;
#[cfg(test)]
mod native_reconstruction_tests;
#[cfg(test)]
mod native_resource_vendor_tests;
pub mod resources;
pub use admission::{
    AdmissionError, AdmissionReport, DocumentAdmission, InputDisposition,
    MAX_EXACT_ADMISSION_BYTES, MAX_EXACT_ADMISSIONS, MissingPrerequisite,
};

#[cfg(test)]
mod native_mutation_vendor_tests;

#[cfg(any(test, doctest))]
mod native_identity_vendor_tests;

#[cfg(test)]
mod native_scalar_debug_vendor_tests;
