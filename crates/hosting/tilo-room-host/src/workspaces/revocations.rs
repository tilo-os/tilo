//! Revocations the relay's workspaces carry.
//!
//! A person's root revokes a key it made, and any device of theirs announces
//! the signed entry in a workspace it shares with the relay (`IdentityHello`).
//! The relay takes it from there. Every entry joins the relay's revocation
//! log, which every handshake, session and administration door reads, so the
//! key is refused everywhere at once — a device of the person's, and the
//! stand-in approver this host holds for them, which no workspace lists
//! because a stand-in is no device. Every workspace the relay governs also
//! writes the entries it may carry into its governance: the device leaves the
//! workspace's access and channels, and every member's replica refuses it from
//! the workspace's own history.

use std::time::SystemTime;

use tilo_lock::RwLockRecover as _;
use tilo_user_identity::{RevocationEntry, verify_revocation_entry};

use super::{GovernedWorkspace, Workspaces};

/// Revocations the relay keeps in its log. A key's revocation is a statement
/// its root signed, and the relay refuses a key it was told to forget for as
/// long as it serves the person: past this many, the log holds what it has and
/// says so rather than grow without bound.
const MAX_LOGGED_REVOCATIONS: usize = 4096;

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
        self.record(&entries)?;
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

    /// Add `entries` to the relay's revocation log and its live list: what
    /// this host refuses from each entry's moment on.
    ///
    /// A root signs only about the keys it made, so whose root signed a
    /// verified entry decides whose keys it reaches, and the relay keeps them
    /// all: its own person's devices, a member's, and the stand-in approver it
    /// holds for a member whose root lives in one browser tab — a key no
    /// workspace lists, because a stand-in is no device, but the one this host
    /// signs with when it approves a returning sign-in.
    fn record(&self, entries: &[RevocationEntry]) -> Result<(), String> {
        let host = &self.inner.host;
        let state_root = host.access.state_root();
        let live = self.revocations();
        let fresh = entries
            .iter()
            .filter(|entry| {
                !live.is_revoked_at(&entry.revoked_device_public_key, entry.revoked_at_ns)
            })
            .collect::<Vec<_>>();
        if fresh.is_empty() {
            return Ok(());
        }
        let logged =
            crate::store::load_revocations(state_root).map_err(|error| error.to_string())?;
        let mut room = MAX_LOGGED_REVOCATIONS.saturating_sub(logged.len());
        let mut changed = false;
        for entry in fresh {
            if room == 0 {
                tracing::warn!("the relay's revocation log is full: a revocation was not kept");
                break;
            }
            if crate::store::append_revocation(state_root, entry)
                .map_err(|error| error.to_string())?
            {
                room = room.saturating_sub(1);
            }
            changed |= host
                .revocations
                .write_recover()
                .insert(entry.clone())
                .map_err(|error| error.to_string())?;
            self.inner
                .agent_door
                .forget(&entry.revoked_device_public_key);
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
