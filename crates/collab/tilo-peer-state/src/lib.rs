//! Shared native/browser neighbour wire and certified state admission.
pub mod governance;
pub mod join_ledger;
/// The workspace invitation and join contract (`tilo-workspace-join`).
pub use tilo_workspace_join as join;
// The wire message admission speaks; owned by `neighbour-wire`.
use neighbour_wire::NeighbourMsg;
#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod receipt_tests;
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
mod cached_document;
pub mod document;
pub mod installation;
mod postcard_sink;
pub mod room;
mod shared;
mod state;
pub use installation::{AppearancePolicy, ArmedRecord, DirectoryRef, Installation, InstallationId};
mod verified_certificate;
pub use state::{
    BoundedPeerError, BoundedPeerInvalid, BoundedStatePeer, DocumentRow, GovernedAccess,
    GovernedAgent, GovernedDelegateRow, GovernedMember, HELD_FOR_SURFACE, InstalledBot,
    LocalRecoveryReservation, MAX_HELD_BYTES, MAX_HELD_INPUTS, MemberAuthority,
    NativePublicationCut, OpenedRecord, PeerDeliveryPolicy, PeerDeliveryPolicyError,
    PeerDocumentCapture, PeerDocumentRead, PeerDocumentReadError, PeerGovernance,
    PeerGovernanceMut, PeerLimits, PeerMediaCall, PeerMediaGovernance, PeerMediaGovernanceMut,
    PeerMember, PeerOutbox, PeerOutboxMut, PeerPolicyBasis, PeerPublication, PeerPublicationMut,
    PeerRead, PeerRevocations, PeerRoomAccess, PeerWorkspace, PeerWorkspaceMut, PlaintextChanges,
    ReceiveOutcome, RecordReaders, RequestedBot, Reseal, SealCheckpoint, SealDelta, SeedDocument,
    SeedState, StatePeer, TransportOrigin, UnmergedReason, UnmergedWrite,
    committed_document_revision, decide_with_surfaces, default_surfaces, peer_handle, seed_room,
    succeeded_authority, workspace_seed,
};
pub mod coverage;
mod governed_routes;
pub use collab_types::DocumentMutation;

pub mod outbox;

pub mod durability;

pub mod topology;

pub mod caller_authority;
pub mod document_import;
pub mod legacy_policy;
pub mod workspace_checkpoint;
pub mod workspace_membership;

mod admission;
pub mod resources;
pub use admission::{
    AdmissionError, AdmissionReport, DocumentAdmission, DropReason, DroppedInput, InputDisposition,
    MAX_EXACT_ADMISSION_BYTES, MAX_EXACT_ADMISSIONS, MissingPrerequisite,
};
