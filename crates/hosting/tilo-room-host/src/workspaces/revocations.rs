//! Revocations the relay's workspaces carry.
//!
//! A person's root revokes a key it made, and any device of theirs announces
//! the signed entry in a workspace it shares with the relay (`IdentityHello`).
//! The relay takes it from there. An entry reaches only keys its own root
//! certified: one naming a device the relay knows certified by another root
//! is dropped, and every list the relay keeps asks a revocation of the root
//! that certified the key. Every other entry joins the relay's revocation
//! log, which every handshake, session and administration door reads, so the
//! key is refused everywhere at once — a device of the person's, and the
//! stand-in approver this host holds for them, which no workspace lists
//! because a stand-in is no device. Every workspace the relay governs also
//! writes the entries it may carry into its governance: the device leaves the
//! workspace's access and channels, and every member's replica refuses it from
//! the workspace's own history.

use std::time::SystemTime;

use tilo_lock::RwLockRecover as _;
use tilo_user_identity::{DeviceCert, RevocationEntry, verify_revocation_entry};

use super::{GovernedWorkspace, Workspaces};

/// Revocations the relay keeps in its log. A key's revocation is a statement
/// its root signed, and the relay refuses a key it was told to forget for as
/// long as it serves the person: past this many, the log holds what it has and
/// says so rather than grow without bound.
const MAX_LOGGED_REVOCATIONS: usize = 4096;

impl Workspaces {
    /// Take in the root-signed `entries` a device announced. An entry
    /// naming a device this relay knows certified by another root is
    /// another person's key, which the entry's root has no say over: it is
    /// dropped.
    pub(super) async fn take_revocations(
        &self,
        entries: Vec<RevocationEntry>,
    ) -> Result<(), String> {
        let workspaces = self
            .inner
            .workspaces
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut known = vec![self.inner.host.cert.clone()];
        for workspace in &workspaces {
            known.extend(workspace.lock().await.peer().certificates());
        }
        let entries = entries
            .into_iter()
            .filter(|entry| verify_revocation_entry(entry).is_ok() && !crosses_roots(entry, &known))
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(());
        }
        self.record(&entries)?;
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
                !live.is_revoked_at(
                    &entry.user_public_key,
                    &entry.revoked_device_public_key,
                    entry.revoked_at_ns,
                )
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
            // The door holds only the host's own person's devices.
            if entry.user_public_key == host.cert.user_public_key {
                self.inner
                    .agent_door
                    .forget(&entry.revoked_device_public_key);
            }
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
    /// governing device, which would leave it without a writer, nor one
    /// naming a device it certifies to another root.
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
            .filter(|entry| !crosses_roots(entry, &certified))
            .filter(|entry| {
                entry.user_public_key == authority.user_public_key
                    || certified.iter().any(|cert| {
                        (cert.device_public_key, cert.user_public_key)
                            == (entry.revoked_device_public_key, entry.user_public_key)
                    })
            })
            .filter(|entry| {
                !carried
                    .iter()
                    .filter(|known| {
                        (known.user_public_key, known.revoked_device_public_key)
                            == (entry.user_public_key, entry.revoked_device_public_key)
                    })
                    .any(|known| known.revoked_at_ns <= entry.revoked_at_ns)
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

/// Whether `entry` names a device one of `certificates` gives to a root other
/// than the one that signed it.
fn crosses_roots(entry: &RevocationEntry, certificates: &[DeviceCert]) -> bool {
    certificates
        .iter()
        .filter(|cert| cert.device_public_key == entry.revoked_device_public_key)
        .any(|cert| cert.user_public_key != entry.user_public_key)
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::{
        DeviceCapabilities, RevocationEntry, RevocationReason, sign_device_cert,
        sign_revocation_entry,
    };

    use super::super::fixtures::{admit_view_reader, governor, test_host};

    fn revocation(root: &SigningKey, device: [u8; 32]) -> RevocationEntry {
        let mut entry = RevocationEntry {
            user_public_key: root.verifying_key().to_bytes(),
            revoked_device_public_key: device,
            revoked_at_ns: tilo_clock::unix_now_ns(),
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, root).unwrap();
        entry
    }

    /// Bob's root signs an entry naming a device of Ada's: the host keeps
    /// nothing of it, moves no admission and no workspace carries it. Ada's
    /// own root's entry still revokes the device everywhere.
    #[tokio::test]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the relay a test binds is kept until it ends; dropping it earlier closes the host the test drives"
    )]
    async fn another_roots_revocation_of_a_device_is_not_taken() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let relay = test_host(dir.path(), &key, &cert).await;
        let workspaces = relay.workspaces();
        let created = workspaces.create("Plans").await.unwrap();
        let ada = SigningKey::from_bytes(&[1; 32]);
        let mut laptop = cert.clone();
        laptop.device_public_key = SigningKey::from_bytes(&[3; 32]).verifying_key().to_bytes();
        laptop.capabilities = DeviceCapabilities::member();
        sign_device_cert(&mut laptop, &ada).unwrap();
        admit_view_reader(workspaces, &created.workspace, &key, &laptop).await;
        let workspace = workspaces.workspace(&created.workspace).await.unwrap();

        let epoch = crate::authority_epoch::current();
        let bob = SigningKey::from_bytes(&[7; 32]);
        workspaces
            .take_revocations(vec![revocation(&bob, laptop.device_public_key)])
            .await
            .unwrap();
        assert!(
            crate::store::load_revocations(dir.path())
                .unwrap()
                .is_empty()
        );
        assert!(workspaces.revocations().is_empty());
        assert!(!workspaces.revocations().refuses(&laptop));
        assert_eq!(
            crate::authority_epoch::current(),
            epoch,
            "no session is re-checked"
        );
        assert!(workspace.lock().await.peer().revocations().is_empty());

        let own = revocation(&ada, laptop.device_public_key);
        workspaces
            .take_revocations(vec![own.clone()])
            .await
            .unwrap();
        assert_eq!(
            crate::store::load_revocations(dir.path()).unwrap(),
            vec![own.clone()]
        );
        assert!(workspaces.revocations().refuses(&laptop));
        assert_ne!(crate::authority_epoch::current(), epoch);
        assert_eq!(workspace.lock().await.peer().revocations(), vec![own]);
        drop(workspace);
        relay.shutdown().await;
    }
}
