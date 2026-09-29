//! Shared native/browser neighbour wire and certified state admission.
pub mod colorimetry;
pub mod governance;
pub mod hdr_metadata;
pub mod join;
pub mod protocol;
pub use protocol::NeighbourMsg;
#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod comment_tests;
#[cfg(test)]
mod direct_room_tests;
#[cfg(test)]
mod public_room_tests;
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
    GovernedMember, LocalRecoveryReservation, NativePublicationCut, PeerDeliveryPolicy,
    PeerDeliveryPolicyError, PeerDocumentRead, PeerDocumentReadError, PeerLimits, PeerMediaCall,
    PeerMember, PeerPolicyBasis, PeerRead, PeerRevocations, PeerRoomAccess, ReceiveOutcome,
    SeedDocument, SeedState, StatePeer, TransportOrigin, VerifiedSecurityMetadataOnly,
    durable_keeps_exact_admissions, peer_handle, seed_room,
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

pub mod retained_proof;

pub mod desktop_delivery;

mod admission;
pub mod resources;
pub use admission::{
    AdmissionError, AdmissionReport, DocumentAdmission, InputDisposition,
    MAX_EXACT_ADMISSION_BYTES, MAX_EXACT_ADMISSIONS, MissingPrerequisite,
};

/// Derive the stable numeric host/gossip shard for a room UUID. This is shared
/// by every room holder, the relay and its admin CLI so local workspace handles never leak onto the
/// wire and operators cannot accidentally bind a different shard.
///
/// # Errors
///
/// Returns an error if `room` is not a valid UUID string.
pub fn room_workspace_id(room: &str) -> Result<u64, uuid::Error> {
    let room = uuid::Uuid::parse_str(room)?;
    // Encode on the stack: bounded delivery paths derive this id under a
    // native allowance that funds no heap allocation.
    let mut canonical = [0u8; 36];
    let hash = blake3::derive_key(
        "tilo-room-transport-v1",
        room.hyphenated().encode_lower(&mut canonical).as_bytes(),
    );
    let mut first8 = [0u8; 8];
    first8.copy_from_slice(&hash[..8]);
    let id = u64::from_le_bytes(first8);
    Ok(if id == 0 { 1 } else { id })
}

/// Match a retained or subscribed binding without aliasing arbitrary numeric IDs.
pub fn room_workspace_id_matches(room: &str, workspace: u64) -> bool {
    room_workspace_id(room).is_ok_and(|id| id == workspace)
}
