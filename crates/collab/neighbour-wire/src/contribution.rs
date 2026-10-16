//! Bounded wire vocabulary for governor-attested document contributions.
//!
//! These values carry evidence; none grants authority merely by decoding.
//! Receivers verify the enrolled governor or an explicit room-scoped
//! delegation, the exact governed-room bases, and the referenced operation.

use collab_types::Signature;
use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

pub const CONTRIBUTION_CHECKPOINT_VERSION: u16 = 1;
pub const CONTRIBUTION_DELEGATION_VERSION: u16 = 1;
pub const MAX_CONTRIBUTION_CHECKPOINT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_CONTRIBUTION_RECEIPTS: usize = 65_536;
pub const MAX_CONTRIBUTION_CHANGES_PER_OPERATION: usize = 4_096;
pub const MAX_CONTRIBUTION_BASIS_HEADS: usize = 256;

/// A governor-signed grant to attest contributions for exactly one room.
/// This is deliberately separate from invitation/admission delegation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionAttestationDelegationV1 {
    pub version: u16,
    pub room: String,
    pub workspace_id: u64,
    pub authority_device_public_key: [u8; 32],
    pub delegate: DeviceCert,
    pub nonce: [u8; 16],
    pub expires_at_ns: Option<u64>,
    pub signature: Signature,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContributionAttestorV1 {
    Governor {
        certificate: Box<DeviceCert>,
    },
    DelegatedHost {
        certificate: Box<DeviceCert>,
        delegation: Box<ContributionAttestationDelegationV1>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContributionContractV1 {
    pub digest: [u8; 32],
    /// Stable action permission discriminant, validated by peer-state.
    pub permission: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContributionRoomBasisV1 {
    /// Exact governed workspace graph at the operation's original admission.
    /// Task Center lives in this graph, not in a synthetic app document.
    pub heads: Vec<[u8; 32]>,
    pub digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContributionTaskFrontierV1 {
    pub task_id: String,
    pub agent: String,
    pub run_epoch: u64,
    pub direction_revision: u64,
}

/// A Conversation reply is bound to its canonical parent and delivery attempt,
/// not to a fabricated Task frontier. Its completion invocation is retained
/// separately on the receipt.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContributionConversationReplyV1 {
    pub room: String,
    pub chat_record: String,
    pub channel: String,
    pub message: String,
    pub request_invocation: [u8; 16],
    pub delivery_invocation: [u8; 16],
    pub agent: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ContributionPurposeV1 {
    Task(ContributionTaskFrontierV1),
    ConversationReply(ContributionConversationReplyV1),
}

/// Closed original invocation policy retained by the owner at admission.
/// This is evidence, not a capability granted to the receiving application.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ContributionOriginalPolicyV1 {
    CertifiedPerson { foreign_restriction: bool },
    ManagedAgent,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContributionReceiptV1 {
    pub change_hash: [u8; 32],
    pub original_certificate: u32,
    pub original_policy: ContributionOriginalPolicyV1,
    pub actor: String,
    pub participant: Option<String>,
    pub source_invocation: [u8; 16],
    pub action: String,
    pub contract: u32,
    pub purpose: Option<ContributionPurposeV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContributionOperationV1 {
    pub app: String,
    pub record: String,
    pub artifact: [u8; 16],
    /// BLAKE3 digest of the exact canonical Person-signed `SignedOp`.
    pub operation_id: [u8; 32],
    pub outer_signer: [u8; 32],
    /// Index into the exact original owner-turn governed-room bases.
    pub room_basis: u32,
    pub receipts: Vec<ContributionReceiptV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContributionCheckpointBodyV1 {
    pub version: u16,
    pub room: String,
    pub workspace_id: u64,
    pub generation: u64,
    pub previous_digest: Option<[u8; 32]>,
    pub authored_at_ns: u64,
    /// Membership, application authority and Task frontiers are evaluated at
    /// each operation's original governed-room basis, never today's graph.
    pub room_bases: Vec<ContributionRoomBasisV1>,
    pub certificates: Vec<DeviceCert>,
    pub contracts: Vec<ContributionContractV1>,
    pub operations: Vec<ContributionOperationV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentContributionCheckpointV1 {
    pub body: ContributionCheckpointBodyV1,
    pub attestor: ContributionAttestorV1,
    pub signature: Signature,
}
