//! Cryptographic and canonical validation for contribution evidence.

pub use neighbour_wire::contribution::{
    ContributionAttestationDelegationV1, ContributionAttestorV1, ContributionCheckpointBodyV1,
    ContributionContractV1, ContributionConversationReplyV1, ContributionOperationV1,
    ContributionOriginalPolicyV1, ContributionPurposeV1, ContributionReceiptV1,
    ContributionRoomBasisV1, ContributionTaskFrontierV1, DocumentContributionCheckpointV1,
};

mod checkpoint;
mod delegation;

pub use checkpoint::{
    contribution_room_basis_digest, sign_contribution_checkpoint,
    verify_contribution_checkpoint_signature,
};
pub use delegation::{sign_contribution_delegation, verify_contribution_delegation};

#[cfg(test)]
mod tests;
