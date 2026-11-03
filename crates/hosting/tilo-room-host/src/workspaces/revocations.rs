//! Revocations the relay's workspaces carry.
//!
//! A person's root revokes one of their devices, and any device announces
//! the signed entry in a workspace it shares with the relay (`IdentityHello`).
//! The relay takes it from there. An entry its own person's root signed joins
//! the relay's revocation log, which every handshake, session and
//! administration door reads, so the device is refused everywhere at once.
//! Every workspace the relay governs writes the entries it may carry into its
//! governance: the device leaves the workspace's access and channels, and every
//! member's replica refuses it from the workspace's own history.

use std::time::SystemTime;

use tilo_lock::RwLockRecover as _;
use tilo_user_identity::{RevocationEntry, verify_revocation_entry};

use super::{GovernedWorkspace, Workspaces};

impl Workspaces {
    /// Take in the root-signed `entries` a device announced.
    pub(super) async fn take_revocations(
        &self,
        entries: Vec<RevocationEntry>,
    ) -> Result<(), String> {
        let entries = entries
            .into_iter()
            .filter(|entry| verify_revocation_entry(entry).is_ok())
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(());
        }
        self.record_own(&entries)?;
        let workspaces = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut refused = Ok(());
        for workspace in workspaces {
            let mut workspace = workspace.lock().await;
            if let Err(error) = self.carry_revocations(&mut workspace, &entries).await {
                tracing::warn!(workspace = %workspace.workspace(), %error, "a workspace did not take a revocation");
                refused = Err(error);
            }
        }
        refused
    }

    /// Add the entries of the relay's own person to its revocation log and
    /// its live list.
    fn record_own(&self, entries: &[RevocationEntry]) -> Result<(), String> {
        let host = &self.inner.host;
        let live = self.revocations();
        let own = entries.iter().filter(|entry| {
            entry.user_public_key == host.cert.user_public_key
                && !live.is_revoked_at(&entry.revoked_device_public_key, entry.revoked_at_ns)
        });
        let mut changed = false;
        for entry in own {
            crate::store::append_revocation(host.access.state_root(), entry)
                .map_err(|error| error.to_string())?;
            changed |= host
                .revocations
                .write_recover()
                .insert(entry.clone())
                .map_err(|error| error.to_string())?;
        }
        if changed {
            crate::authority_epoch::bump();
        }
        Ok(())
    }

    /// Write into `workspace`'s governance every entry of `entries` (and of the
    /// relay's live list) it may carry and does not yet: those its
    /// authority's root signed, and those of a root the workspace certifies the
    /// revoked device to. A workspace never carries the revocation of its own
    /// governing device, which would leave it without a writer.
    pub(super) async fn carry_revocations(
        &self,
        workspace: &mut GovernedWorkspace,
        entries: &[RevocationEntry],
    ) -> Result<(), String> {
        let key = &self.inner.host.key;
        let authority = workspace.record.authority.clone();
        let certified = workspace.peer().certificates();
        let carried = workspace.peer().revocations();
        let live = self.revocations();
        let carry = entries
            .iter()
            .chain(live.iter())
            .filter(|entry| entry.revoked_device_public_key != authority.device_public_key)
            .filter(|entry| {
                entry.user_public_key == authority.user_public_key
                    || certified.iter().any(|cert| {
                        (cert.device_public_key, cert.user_public_key)
                            == (entry.revoked_device_public_key, entry.user_public_key)
                    })
            })
            .filter(|entry| {
                !carried.iter().any(|known| {
                    known.revoked_device_public_key == entry.revoked_device_public_key
                        && known.revoked_at_ns <= entry.revoked_at_ns
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if carry.is_empty() {
            return Ok(());
        }
        let now = SystemTime::now();
        let mut candidate = workspace.peer().clone();
        if candidate
            .governance_mut()
            .publish_governed_revocations(&carry, key, now)?
        {
            workspace.commit(candidate, key, now, |record, _| {
                Ok(record.collaboration.clone())
            })?;
            tracing::info!(workspace = %workspace.workspace(), revocations = carry.len(), "workspace carries revocations");
        }
        self.settle(workspace).await
    }
}
