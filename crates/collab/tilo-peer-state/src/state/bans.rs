//! Banning: a removal plus the `block:<person>` Deny that keeps the person
//! out until the workspace unbans them. Admission refuses a banned person
//! ([`crate::governance::admit_invited_member`]).

use super::SystemTime;
use super::facades::PeerGovernanceMut;
use collab_types::signer::DeviceSigner;
use tilo_action_policy::{Grantee, roles};

impl PeerGovernanceMut<'_> {
    /// Remove `user_public_key` (when a member) with every device of theirs,
    /// and keep them out until [`Self::unban_member`]. `by` is the person
    /// who decided it, recorded on the block.
    ///
    /// # Errors
    /// Returns an error if the change is not `key`'s device's to make, the person is
    /// the structural owner, or governance cannot be updated.
    pub fn ban_member(
        self,
        user_public_key: [u8; 32],
        by: &str,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let person = hex::encode(user_public_key);
        // Work on a clone: a refused block must not leave the removal applied.
        let mut candidate = self.peer.clone();
        let removed = candidate
            .governance_mut()
            .remove_member(user_public_key, key, now)?;
        let block = roles::block_grant(&self.peer.core.workspace, &person, Grantee::person(by));
        let blocked = candidate
            .governance_mut()
            .put_governed_grants(&[block], key, now)?;
        *self.peer = candidate;
        Ok(removed || blocked)
    }
    /// Let a banned person be admitted again: revoke their block.
    ///
    /// # Errors
    /// Returns an error if the change is not `key`'s device's to make or governance
    /// cannot be updated.
    pub fn unban_member(
        self,
        user_public_key: [u8; 32],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let id = roles::block_grant_id(&hex::encode(user_public_key));
        self.peer
            .governance_mut()
            .remove_governed_grants(&[id], key, now)
    }
}
