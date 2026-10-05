//! Cryptographic and canonical validation for contribution evidence.

mod checkpoint;
mod delegation;

pub use checkpoint::{
    contribution_room_basis_digest, sign_contribution_checkpoint,
    verify_contribution_checkpoint_signature,
};
pub use delegation::{sign_contribution_delegation, verify_contribution_delegation};

#[cfg(test)]
mod tests;
