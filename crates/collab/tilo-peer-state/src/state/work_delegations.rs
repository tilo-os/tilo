//! A member's signed delegation of their own work to a server device.
//!
//! Work in a workspace is the workspace's or a member's. A key holder takes the
//! workspace's work itself; a member's work — an agent using their mail, a
//! Browser they are signed in to, an outbox entry that needs their password —
//! is taken by a server of another person only because that member delegated
//! it. The delegation is a resource of the workspace's own state
//! ([`collab_types::work_delegation::WorkDelegation`]): the member's device
//! signs it, names the server, this workspace, the work by the grant paths the
//! workspace decides actions by and until when, and every holder replays the
//! same rule. A revocation is the same delegation signed again with the revocation
//! set, so it stops the work on every holder at once and needs no one to
//! delete a row.
//!
//! A key holder that takes a member's work under a live delegation signs what
//! it writes with its own device key; every receiver admits the change as the
//! member's, bounded by the delegation's paths
//! ([`crate::StatePeer::authorize_document`]). A key holder without one is
//! refused, as any device holding no member's authority is.
use super::facades::{PeerWorkspace, PeerWorkspaceMut};
use super::{ROOT, SystemTime, device_rows, now_ns};
use crate::state::copies::CopyRole;
use automerge::{AutoCommit, ReadDoc as _, Value};
use collab_types::signer::DeviceSigner;
use collab_types::work_delegation::{WORK_DELEGATIONS_KEY, WorkDelegation};

/// The workspace document's id, as its founder wrote it.
fn workspace_of(doc: &AutoCommit) -> Option<String> {
    match doc.get(ROOT, "workspace").ok()??.0 {
        Value::Scalar(value) => value.as_str().map(str::to_owned),
        Value::Object(_) => None,
    }
}

/// The work delegations `doc` holds, one per row, ordered by row key. A row
/// whose key names another server or member than the delegation, or whose
/// signature does not verify, is left out: whoever wrote it, only the
/// member's own signature says what a server may take.
#[must_use]
pub fn work_delegations_of(doc: &AutoCommit) -> Vec<WorkDelegation> {
    let Some(workspace) = workspace_of(doc) else {
        return Vec::new();
    };
    let mut delegations: Vec<WorkDelegation> =
        device_rows::all_rows(Some(doc), WORK_DELEGATIONS_KEY)
            .into_iter()
            .filter_map(|(server, person, json)| {
                let delegation: WorkDelegation = serde_json::from_str(&json).ok()?;
                (hex::encode(delegation.server) == server
                    && delegation.person == person
                    && delegation.verify_signature(&workspace).is_ok())
                .then_some(delegation)
            })
            .collect();
    delegations.sort_by_key(WorkDelegation::row_key);
    delegations
}

impl PeerWorkspace<'_> {
    /// The work delegations the workspace holds, live or not, each as the
    /// member's device signed it: what every member reads of the work they
    /// gave a server, beside who holds the workspace.
    #[must_use]
    pub fn work_delegations(self) -> Vec<WorkDelegation> {
        self.peer
            .core
            .workspace_doc
            .as_deref()
            .map_or_else(Vec::new, work_delegations_of)
    }

    /// The delegation under which `server` takes `person`'s work at `now`, if
    /// one is live: signed for this workspace, inside its window, not revoked,
    /// signed by a current certified device of that member, and for a server
    /// the workspace names among its key-holding holders. A server named by a
    /// delegation but no longer holding the keys takes nothing under it.
    #[must_use]
    pub fn work_delegation(
        self,
        server: &[u8; 32],
        person: &[u8; 32],
        now: SystemTime,
    ) -> Option<WorkDelegation> {
        let now_ns = now_ns(now);
        let workspace = self.peer.core.workspace.clone();
        let delegation = self
            .work_delegations()
            .into_iter()
            .filter(|delegation| &delegation.server == server && &delegation.person == person)
            .filter(|delegation| delegation.verify(&workspace, now_ns).is_ok())
            .max_by_key(|delegation| delegation.issued_at_ns)?;
        // The signing device must be the member's own current device: the
        // delegation says whose it is, and the workspace's roster says whether
        // that is true.
        let cert = self.peer.certificate(&delegation.device, now).ok()?;
        if cert.user_public_key != delegation.person {
            return None;
        }
        self.peer.membership(delegation.device, now).ok()?;
        (self.peer.governance().holder_role(server) == Some(CopyRole::KeyHolder))
            .then_some(delegation)
    }

    /// The delegation under which `server` takes a member's work at `now`: the
    /// one with the latest issue time among the live ones.
    #[must_use]
    pub fn delegation_for_server(
        self,
        server: &[u8; 32],
        now: SystemTime,
    ) -> Option<WorkDelegation> {
        let now_ns = now_ns(now);
        let workspace = self.peer.core.workspace.clone();
        self.work_delegations()
            .into_iter()
            .filter(|delegation| &delegation.server == server)
            .filter(|delegation| delegation.verify(&workspace, now_ns).is_ok())
            .max_by_key(|delegation| delegation.issued_at_ns)
            .filter(|delegation| {
                self.work_delegation(server, &delegation.person, now)
                    .is_some()
            })
    }
}

impl PeerWorkspaceMut<'_> {
    /// Write `delegation` into the workspace, authored by the member's device
    /// `key` that signed it. Writing it as it is written changes nothing.
    ///
    /// # Errors
    /// It is not `key`'s device's of a current member, the delegation
    /// is not current, `server` is no key-holding holder of this workspace, or
    /// the workspace cannot be written.
    pub fn author_work_delegation(
        mut self,
        delegation: &WorkDelegation,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let workspace = self.peer.core.workspace.clone();
        // A revocation is the same delegation signed again, so what it must be
        // is its signature and its window: one revoked now still names the
        // delegation it stops.
        delegation
            .verify_signature(&workspace)
            .map_err(|error| error.to_string())?;
        let at = now_ns(now);
        if at < delegation.issued_at_ns || at >= delegation.expires_at_ns {
            return Err("the delegation is not current".into());
        }
        if delegation.device != key.device_public_key() {
            return Err("a delegation is signed by the member's own device".into());
        }
        let cert = self
            .peer
            .certificate(&delegation.device, now)
            .map_err(|error| error.to_string())?;
        if cert.user_public_key != delegation.person {
            return Err("the signing device is not the member's".into());
        }
        self.peer.membership(delegation.device, now)?;
        if self.peer.governance().holder_role(&delegation.server) != Some(CopyRole::KeyHolder) {
            return Err("the delegation names no key holder of this workspace".into());
        }
        if self
            .peer
            .workspace()
            .work_delegations()
            .contains(delegation)
        {
            return Ok(false);
        }
        let json = serde_json::to_string(delegation).map_err(|error| error.to_string())?;
        self.write_row(
            WORK_DELEGATIONS_KEY,
            &delegation.row_key(),
            &delegation.device,
            Some(&json),
        )?;
        Ok(true)
    }

    /// Revoke `server`'s delegation of `person`'s work at `now`, as that
    /// member's device `key`: from then on every holder refuses the work the
    /// server takes for them. Returns whether a live delegation was revoked;
    /// `false` when the member delegated nothing to it, or already revoked it.
    ///
    /// # Errors
    /// `key` is not the member's device that signed the delegation, or the
    /// workspace cannot be written.
    pub fn revoke_work_delegation(
        self,
        server: &[u8; 32],
        person: &[u8; 32],
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, String> {
        let Some(delegation) = self
            .peer
            .workspace()
            .work_delegations()
            .into_iter()
            .find(|delegation| &delegation.server == server && &delegation.person == person)
        else {
            return Ok(false);
        };
        if delegation.revoked_at_ns.is_some() {
            return Ok(false);
        }
        if delegation.device != key.device_public_key() {
            return Err("only the member's device that signed it revokes it".into());
        }
        let revoked = delegation
            .revoked(key, now_ns(now))
            .map_err(|error| error.to_string())?;
        self.author_work_delegation(&revoked, key, now)
    }
}
