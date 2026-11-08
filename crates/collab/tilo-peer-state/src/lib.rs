//! Shared native/browser neighbour wire and certified state admission.
#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::arc_with_non_send_sync,
        reason = "a page's store and neighbour handles reach JavaScript and stay on the page's one thread"
    )
)]
#[cfg(not(target_arch = "wasm32"))]
mod frames;
pub mod governance;
#[cfg(not(target_arch = "wasm32"))]
pub mod join_answer;
pub mod join_ledger;
#[cfg(not(target_arch = "wasm32"))]
pub mod join_transport;
/// The workspace invitation and join contract (`tilo-workspace-join`).
pub use tilo_workspace_join as join;
// The wire message admission speaks; owned by `neighbour-wire`.
use neighbour_wire::NeighbourMsg;
pub use neighbour_wire::{RefusalReason, WriteRefusal};
#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod receipt_tests;
#[cfg(test)]
mod shared_desktop_tests;
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
/// Actions between the devices of a workspace, with no host in between.
#[cfg(not(target_arch = "wasm32"))]
pub mod actions;
mod cached_document;
#[cfg(not(target_arch = "wasm32"))]
pub mod direct;
pub mod document;
pub mod document_frame;
pub mod installation;
/// Running an offloadable invocation on another device, through the offer
/// round.
pub mod offload;
pub mod placement;
mod postcard_sink;
mod shared;
mod state;
pub mod workspace;
pub use installation::{AppearancePolicy, ArmedRecord, DirectoryRef, Installation, InstallationId};
mod verified_certificate;
/// The person's own documents between their devices, with no host in between.
#[cfg(not(target_arch = "wasm32"))]
pub use state::{AdmittedFrame, PERSONAL_ALPN, PersonalSync};
pub use state::{
    DelegateInactive, DocumentRow, GovernedAccess, GovernedAgent, GovernedDelegateRow,
    GovernedMember, HELD_FOR_SURFACE, InstalledApp, InstalledBot, MAX_HELD_BYTES, MAX_HELD_INPUTS,
    MemberAuthority, MembershipError, NativePublicationCut, OpenedRecord, PeerDocumentCapture,
    PeerDocumentReadError, PeerGovernance, PeerGovernanceMut, PeerMediaGovernance,
    PeerMediaGovernanceMut, PeerOutbox, PeerOutboxMut, PeerPersonal, PeerPublication,
    PeerPublicationMut, PeerWorkspace, PeerWorkspaceMut, PersonalStore, PlaintextChanges,
    ReceiveOutcome, RecordReaders, RequestedBot, Reseal, SealCheckpoint, SealDelta, SeedDocument,
    SeedState, StatePeer, TransportOrigin, UnmergedReason, UnmergedWrite, WORKSPACE_HOSTS_KEY,
    committed_document_revision, decide_with_surfaces, default_surfaces, peer_handle,
    seed_workspace, workspace_host_actor, workspace_hosts_of, workspace_seed,
};
pub mod coverage;
mod governed_routes;
pub use collab_types::DocumentMutation;

pub mod outbox;
pub mod owner;
pub mod owner_commit;

pub mod durability;

pub mod document_import;
pub mod legacy_policy;
/// A workspace's membership: the signed operations it is made of, and the
/// membership every device derives from them.
pub mod members;
pub mod workspace_checkpoint;
pub mod workspace_membership;

mod admission;
pub mod resources;
pub use admission::{
    AdmissionError, AdmissionReport, DocumentAdmission, DropReason, DroppedInput, InputDisposition,
    MAX_EXACT_ADMISSION_BYTES, MAX_EXACT_ADMISSIONS, MissingPrerequisite,
};
