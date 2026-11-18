//! A member's device writing its delegation of work to a server into the
//! workspace.
//!
//! Work in a workspace is the workspace's or a member's. A server of another
//! person takes a member's work only under the delegation that member's own
//! device signed ([`collab_types::work_delegation::WorkDelegation`]), which
//! names the server, this workspace, the work by the grant paths it is decided
//! by, the actor the server acts as, and the vault items of theirs it may use.
//! The delegation is a row of the workspace's own document, so every holder
//! replays the same rule and the member's revocation — the same delegation
//! signed again, with the revocation set — stops the work at once everywhere.
//!
//! A member's device carries the workspace service too. On a device that holds
//! the workspace, its own or a server's, this authors the row as that device
//! signed it. The row's author is the member's device; the change that carries
//! it here is the holding device's, and every reader admits the row by the
//! member's own signature and the roster that says the signing device is
//! theirs.

use ed25519_dalek::SigningKey;

use super::Workspaces;
use collab_types::work_delegation::WorkDelegation;

impl Workspaces {
    /// Author `delegation` into `workspace`, as the member's own device `key`
    /// signed it: from then on this holder admits the work it names the server
    /// for, and no more than it names. Authoring it as it already stands
    /// changes nothing.
    ///
    /// # Errors
    /// The workspace is not held here, `key` is not the device that signed it,
    /// the signing device is not a member's current device, the delegation is
    /// not current, or it names no key-holding holder of this workspace.
    pub async fn author_work_delegation(
        &self,
        workspace: &str,
        delegation: &WorkDelegation,
        key: &SigningKey,
    ) -> Result<bool, String> {
        let held = self
            .workspace(workspace)
            .await
            .map_err(|error| format!("{error}"))?;
        let mut governed = held.lock().await;
        let mut candidate = governed.peer().clone();
        let now = tilo_clock::wall();
        let changed = candidate
            .workspace_mut()
            .author_work_delegation(delegation, key, now)?;
        if !changed {
            return Ok(false);
        }
        let collaboration = governed.collaboration().clone();
        let own = self.inner.host.key.clone();
        governed
            .commit(candidate, &own, now, |_, _| Ok(collaboration))
            .map(drop)?;
        drop(governed);
        Ok(true)
    }

    /// Revoke `server`'s delegation of `person`'s work in `workspace` at
    /// `now`, as that member's device `key`: the same delegation signed again
    /// with the revocation set, so every holder stops the server's work for
    /// them at once. Answers whether a live delegation was revoked.
    ///
    /// # Errors
    /// The workspace is not held here, `key` is not the device that signed the
    /// delegation, or the revocation cannot be written.
    pub async fn revoke_work_delegation(
        &self,
        workspace: &str,
        server: &[u8; 32],
        person: &[u8; 32],
        key: &SigningKey,
    ) -> Result<bool, String> {
        let held = self
            .workspace(workspace)
            .await
            .map_err(|error| format!("{error}"))?;
        let mut governed = held.lock().await;
        let mut candidate = governed.peer().clone();
        let now = tilo_clock::wall();
        let changed = candidate
            .workspace_mut()
            .revoke_work_delegation(server, person, key, now)?;
        if !changed {
            return Ok(false);
        }
        let collaboration = governed.collaboration().clone();
        let own = self.inner.host.key.clone();
        governed
            .commit(candidate, &own, now, |_, _| Ok(collaboration))
            .map(drop)?;
        drop(governed);
        Ok(true)
    }
}
