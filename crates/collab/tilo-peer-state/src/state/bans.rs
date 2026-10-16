//! Banning: a removal plus the `block:<person>` Deny that keeps the person
//! out until the room unbans them. Admission refuses a banned person
//! ([`crate::governance::admit_invited_member`]).

use super::SystemTime;
use super::facades::PeerGovernanceMut;
use ed25519_dalek::SigningKey;
use tilo_action_policy::{Subject, roles};

impl PeerGovernanceMut<'_> {
    /// Remove `user_public_key` (when a member) with every device of theirs,
    /// and keep them out until [`Self::unban_member`]. `by` is the person
    /// who decided it, recorded on the block.
    ///
    /// # Errors
    /// Returns an error if `key` is not the governance writer, the person is
    /// the structural owner, or governance cannot be updated.
    pub fn ban_member(
        self,
        user_public_key: [u8; 32],
        by: &str,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let person = hex::encode(user_public_key);
        // Work on a clone: a refused block must not leave the removal applied.
        let mut candidate = self.peer.clone();
        let removed = candidate
            .governance_mut()
            .remove_member(user_public_key, key, now)?;
        let block = roles::block_grant(&self.peer.core.room, &person, Subject::person(by));
        let blocked = candidate
            .governance_mut()
            .put_governed_grants(&[block], key, now)?;
        *self.peer = candidate;
        Ok(removed || blocked)
    }
    /// Let a banned person be admitted again: revoke their block.
    ///
    /// # Errors
    /// Returns an error if `key` is not the governance writer or governance
    /// cannot be updated.
    pub fn unban_member(
        self,
        user_public_key: [u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let id = roles::block_grant_id(&hex::encode(user_public_key));
        self.peer
            .governance_mut()
            .remove_governed_grants(&[id], key, now)
    }
}
