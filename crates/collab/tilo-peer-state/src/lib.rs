//! Shared native/browser neighbour wire and certified state admission.
pub mod colorimetry;
pub mod contribution_attestation;
pub mod governance;
pub mod hdr_metadata;
pub mod join;
pub mod protocol;
pub use protocol::NeighbourMsg;
#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod receipt_tests;
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
mod cached_document;
pub mod document;
#[cfg(test)]
mod join_contract_tests;
pub mod room;
mod state;
mod verified_certificate;
pub use state::{
    BoundedPeerError, BoundedPeerInvalid, BoundedStatePeer, DocumentRow, GovernedAccess,
    GovernedMember, LocalRecoveryReservation, MemberAuthority, NativePublicationCut,
    PeerDeliveryPolicy, PeerDeliveryPolicyError, PeerDocumentRead, PeerDocumentReadError,
    PeerLimits, PeerMediaCall, PeerMember, PeerPolicyBasis, PeerRead, PeerRevocations,
    PeerRoomAccess, ReceiveOutcome, Reseal, SealCheckpoint, SeedDocument, SeedState, StatePeer,
    TransportOrigin, VerifiedSecurityMetadataOnly, decide_with_surfaces, default_surfaces,
    peer_handle, seed_room,
};
pub mod coverage;
mod governed_routes;
pub mod transport;
pub use collab_types::DocumentMutation;

pub mod outbox;

#[cfg(test)]
mod enrollment_tests;

pub mod enrollment;

pub mod durability;

pub mod topology;

pub mod desktop_command;
pub mod document_import;
pub mod owner_commit;
pub mod workspace_membership;
pub mod workspace_task;

pub mod retained_proof;

pub mod desktop_delivery;

mod admission;
pub mod resources;
pub use admission::{
    AdmissionError, AdmissionReport, DocumentAdmission, InputDisposition,
    MAX_EXACT_ADMISSION_BYTES, MAX_EXACT_ADMISSIONS, MissingPrerequisite,
};
