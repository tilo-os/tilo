//! Banning: a removal plus the `block:<person>` Deny that keeps the person
//! out until the workspace unbans them. Admission refuses a banned person
//! ([`crate::governance::admit_invited_member`]).

use super::SystemTime;
use super::facades::PeerGovernanceMut;
use collab_types::signer::DeviceSigner;
use tilo_action_policy::{Change, roles};

impl PeerGovernanceMut<'_> {
    /// Remove `user_public_key` (when a member) with every device of theirs,
    /// and keep them out until [`Self::unban_member`]. The block records the
    /// person whose device signs it as who decided it.
    ///
    /// # Errors
    /// Returns an error if the change is not `key`'s device's to make, the person is
    /// the structural owner, or governance cannot be updated.
    pub fn ban_member(
        self,
        user_public_key: [u8; 32],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let person = hex::encode(user_public_key);
        // Work on a clone: a refused block must not leave the removal applied.
        let mut candidate = self.peer.clone();
        // The removal and the block are the one ban. The block is written as the ban it is, so
        // every device decides it by who may ban, as a moderator's device signs it, never as a
        // grant only an owner gives.
        let stamp = candidate.own_action(roles::governance::BAN, key, now);
        let (removed, blocked) = candidate.naming(stamp, |peer| {
            let removed = peer
                .governance_mut()
                .remove_member(user_public_key, key, now)?;
            let document = peer
                .core
                .workspace_doc
                .clone()
                .ok_or("workspace governance missing")?;
            let blocked = peer.write_changes(document, vec![Change::Ban { person }], key, now)?;
            Ok::<_, String>((removed, blocked))
        })?;
        *self.peer = candidate;
        Ok(removed || blocked)
    }

    /// Let a banned person be admitted again: revoke their block, written as the unban it is.
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
        let person = hex::encode(user_public_key);
        self.peer.require_member_writer(key, now)?;
        let document = self
            .peer
            .core
            .workspace_doc
            .clone()
            .ok_or("workspace governance missing")?;
        let stamp = self.peer.own_action(roles::governance::UNBAN, key, now);
        self.peer.naming(stamp, |peer| {
            peer.write_changes(document, vec![Change::Unban { person }], key, now)
        })
    }
}
