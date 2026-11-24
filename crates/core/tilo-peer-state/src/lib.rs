//! Shared native/browser neighbour wire and certified state admission.
#![cfg_attr(
    all(target_arch = "wasm32", target_os = "unknown"),
    allow(
        clippy::arc_with_non_send_sync,
        reason = "a page's store and neighbour handles reach JavaScript and stay on the page's one thread"
    ),
    allow(
        clippy::future_not_send,
        reason = "a page's signer answers through a JavaScript promise, so the owner's write future, which awaits it while it holds the page's store, stays on the page's one thread"
    )
)]
mod before_own_removal;
pub use before_own_removal::kept_message;
mod device_answers;
mod frames;
pub mod governance;
#[cfg(not(target_arch = "wasm32"))]
pub mod join_answer;
pub mod join_log;
#[cfg(not(target_arch = "wasm32"))]
pub mod join_transport;
mod kept_states;
#[cfg(not(target_arch = "wasm32"))]
mod link_proof;
#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-support")))]
pub mod loopback;
mod person_records;
#[cfg(all(not(target_arch = "wasm32"), any(test, feature = "test-support")))]
pub mod sealed_records;
/// The workspace invitation and join contract (`tilo-workspace-join`).
pub use tilo_workspace_join as join;
/// What a device the workspace took out of its roster hears when it presents
/// the certificate the workspace still holds: it is kept out because it is off
/// the roster, and only a deliberate admission writes its row back.
pub const REMOVED_DEVICE_REFUSED: &str = "this device was removed from the workspace; add it again";
// The wire message admission speaks; owned by `neighbour-wire`.
pub use neighbour_wire::NeighbourMsg;
pub use neighbour_wire::{RefusalReason, WriteRefusal};
#[cfg(test)]
mod admission_cost_tests;
#[cfg(test)]
mod admission_tests;
#[cfg(test)]
mod receipt_tests;
#[cfg(test)]
mod removed_names_tests;
#[cfg(test)]
mod shared_desktop_tests;
#[cfg(test)]
tilo_automerge_isolate::test_worker!();
/// Actions between the devices of a workspace, with no host in between.
#[cfg(not(target_arch = "wasm32"))]
pub mod actions;
mod cached_document;
/// Two devices of one workspace syncing with no host between them, over
/// whatever carries their link: a native device's iroh connection or a page's
/// WebTransport session ([`transport::Transport`]).
pub mod direct;
pub mod document;
pub mod document_frame;
/// The one call a computer with no certificate makes: asking the device that issued its ticket
/// to have it certified.
#[cfg(not(target_arch = "wasm32"))]
pub mod new_device;
pub mod opening;
mod ordered;
mod postcard_sink;
/// How far this device's own writes have gone: pending until another device keeping the
/// workspace took them in.
pub mod reach;
pub mod replica;
/// A workspace's retained sealed history: the frames a store device keeps
/// without a key to open them.
mod retained;
/// What a device's own roles read and write its workspaces' documents through.
mod shared;
mod state;
pub mod store;
pub mod transport;
mod verified_certificate;
pub mod workspace;
/// The person's answers about the devices of their machines, kept in their own workspace.
pub use device_answers::{
    DEVICE_ANSWERS_KIND, DeviceAnswer, DeviceGrants, Kept, PERSON_RECORDS_APP, Refusal,
};
/// The person's own workspace, as a part of a session reads and writes its documents.
pub use person_records::{DocumentEdit, PersonDocuments};
pub use state::CallGate;
pub use state::ChangeRefused;
/// The durable layout a replica's state is written in ([`StatePeer::durable`]).
pub use state::DURABLE_VERSION;
pub use state::DocumentKeysBasis;
/// A state of a workspace's governance, kept to ask which heads it holds.
pub use state::GovernanceHeads;
pub use state::Quarantined;
pub use state::mail_send_lease;
pub use state::{
    ActionStamp, CLOCK_SKEW_NS, COPY_POLICY_KEY, CopyPolicy, CopyRole, CopyStatus, DONE_KEY,
    DelegateInactive, DocumentRow, Done, GovernedAccess, GovernedAgent, GovernedDelegateRow,
    HELD_COPIES_KEY, HeldCopy, HeldDocument, HolderCopy, InstalledApp, KeptDocument, LEASES_KEY,
    Lease, LeaseClaim, MAX_HELD_BYTES, MAX_HELD_FOR_GOVERNANCE_PER_WRITER, MAX_HELD_INPUTS, Member,
    MemberId, MemberList, MembershipError, MergedDocument, Missing, NotAdmitted, OpenSealedError,
    OpenedDocument, OpenedHistory, OpenedWrite, PeerDocumentCapture, PeerDocumentReadError,
    PeerGovernance, PeerGovernanceMut, PeerOutbox, PeerOutboxMut, PeerPublication,
    PeerPublicationMut, PeerWorkspace, PeerWorkspaceMut, PlacedDevice, ReceiveOutcome, Reseal,
    RowError, SCHEDULES_KEY, SealCheckpoint, SeedDocument, SeedState, Shortfall, StatePeer,
    TransportOrigin, UnmergedReason, UnmergedWrite, WORKSPACE_KEEPING_DEVICES_KEY,
    committed_document_revision, decide_with_surfaces, default_surfaces, holder_role,
    keeping_devices_of, peer_handle, seed_workspace, seed_workspace_at, trigger_mark, unranked,
    workspace_grants_of, workspace_host_actor, workspace_seed,
};
pub use state::{CHANNELS as REGISTRY_CHANNELS, PackageRegistry, RevokedPublisher, TrustAttestor};
pub use state::{CHECKPOINT_CHECKS_KEY, CheckpointCheck};
pub use state::{Captured, changes_since, saved_whole};
pub use state::{IMPORTED_HISTORY_PREFIX, Imported, ImportedHistory};
pub use state::{vm_lease, vm_rank};
pub mod coverage;
mod governed_routes;
pub mod removals;
pub mod standing;
pub use collab_types::DocumentMutation;

mod certified;
#[cfg(test)]
#[path = "certified_tests.rs"]
mod certified_tests;
mod document_read;
mod draft;
#[cfg(not(target_arch = "wasm32"))]
pub mod enroll;
mod enrollment;
pub mod outbox;
pub mod owner;
mod vouched;
pub mod workspace_documents;
pub mod workspace_view;

pub mod durability;

pub mod document_import;
/// A workspace's membership: the signed operations it is made of, and the
/// membership every device derives from them.
pub mod members;
pub mod workspace_membership;

mod admission;
pub mod own_admission;
pub mod resources;
/// The work reading and writing a workspace's history costs, for the whole
/// process.
pub mod work;
pub use admission::{
    AdmissionError, AdmissionReport, CopiedDevice, DocumentAdmission, DropReason, DroppedInput,
    InputDisposition, MAX_EXACT_ADMISSION_BYTES, MAX_EXACT_ADMISSIONS, MissingPrerequisite,
    Released, StillHeld,
};
