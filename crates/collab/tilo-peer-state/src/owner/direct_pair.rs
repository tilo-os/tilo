//! The pair of people a direct conversation belongs to, announced by the
//! device that opens it: the one workspace write that makes `direct/<pair
//! id>` theirs ([`crate::workspace::direct_pair`]). Everybody may derive the
//! record's id; only this proof says whose it is.
use crate::owner::{NativeWorkspaceOwner, WorkspaceId};
use crate::owner_commit::{CommitBinding, CommitTarget};
use ed25519_dalek::SigningKey;
use tilo_user_identity::DeviceCert;
use web_time::SystemTime;

impl NativeWorkspaceOwner {
    /// The pairs the workspace holds a proof of that lasts at `now`, by pair
    /// id, each with its two people in canonical hex: which conversation
    /// records there are to open. Anyone here may know them; only the two
    /// people's devices read or change a record.
    ///
    /// # Errors
    /// The workspace is not the owner's current one.
    pub fn direct_pairs(
        &self,
        workspace: WorkspaceId,
        now: SystemTime,
    ) -> Result<std::collections::BTreeMap<String, [String; 2]>, String> {
        self.with_current(workspace, |view| {
            Ok(view.peer().governance().direct_pairs(now))
        })
    }

    /// Announce the pair this device's person shares with `other`, in
    /// `workspace`, signed by `key`, this device's key: from then on the
    /// pair's record is read and changed by the two people's devices and by
    /// no one else's. Answers whether the workspace changed: a pair it
    /// already holds stays as it is, so the first device to open a
    /// conversation names it for both.
    ///
    /// # Errors
    /// A caller or key that is not this device's, a device the workspace no
    /// longer admits, a full workspace, or failed durable publication.
    pub fn commit_direct_pair(
        &self,
        workspace: WorkspaceId,
        caller: &DeviceCert,
        other: &[u8; 32],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<bool, String> {
        let _turn = self.0.mutation.lock();
        let mut candidate = self.with_current(workspace, |view| Ok(view.clone()))?;
        if candidate.device() != caller
            || key.verifying_key().to_bytes() != caller.device_public_key
        {
            return Err("a pair is announced by this device, with its own key".into());
        }
        let governs = candidate.hosted_here();
        let before = candidate.peer.workspace_heads().unwrap_or_default();
        let mut peer = candidate.peer.clone();
        if !peer
            .governance_mut()
            .author_direct_pair(caller, other, key, now)?
        {
            return Ok(false);
        }
        let next_generation = candidate
            .generation()
            .checked_add(1)
            .ok_or("native pair generation exhausted")?;
        let written = if governs {
            peer.publication_mut().durable_mutation_messages(
                &candidate.peer,
                key,
                now,
                next_generation,
            )?;
            Vec::new()
        } else {
            peer.workspace_changes_since(&before)
        };
        candidate.peer = peer;
        candidate.record.peer = candidate.peer.durable()?;
        candidate.record.generation = next_generation;
        let binding = CommitBinding {
            workspace: workspace.to_string(),
            target: CommitTarget::DirectPair {
                device: caller.device_public_key,
            },
            request_digest: *blake3::hash(other).as_bytes(),
            basis: *blake3::hash(&[workspace.to_string().as_bytes(), other].concat()).as_bytes(),
        };
        if governs {
            crate::owner::publication::replace_governance_checkpoint(&mut candidate, binding, now)?;
        } else if !written.is_empty() {
            crate::owner::publication::add_member_changes(&mut candidate, written, binding, now)?;
        }
        self.publish_workspace(workspace, candidate, None)?;
        Ok(true)
    }
}
