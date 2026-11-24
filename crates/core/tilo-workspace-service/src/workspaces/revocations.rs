//! Revocations this holder's workspaces carry.
//!
//! A person's root revokes a key it made, and any device of theirs announces
//! the signed entry in a workspace it shares with this holder
//! (`IdentityHello`). The holder takes it from there. An entry reaches only
//! keys its own root certified: one naming a device the holder knows certified
//! by another root is dropped, and every list the holder keeps asks a
//! revocation of the root that certified the key. An entry speaks for a root
//! the holder serves, too: a root of its own person or of a member of one of
//! its workspaces. Another root made no key here, so what it announces is
//! dropped unread, and nobody can fill the holder's log with entries that
//! reach nothing. Every entry the holder keeps joins its revocation log, which
//! every handshake, session and administration entry reads, so the key is
//! refused everywhere at once — a device of the person's, and the stand-in
//! approver this holder keeps for them, which no workspace lists because a
//! stand-in is no device. Every workspace the holder governs also writes the
//! entries it may carry into its governance, each with its cut
//! ([`tilo_peer_state::removals`]): the device leaves the workspace's access
//! and keys, every member's replica refuses what it signs from there on, and
//! what the workspace had taken of it before stays the workspace's history.

use tilo_lock::RwLockRecover as _;
use tilo_user_identity::{DeviceCert, RevocationEntry, verify_revocation_entry};

use super::{GovernedWorkspace, Workspaces};

impl Workspaces {
    /// Take in the root-signed `entries` a device announced. An entry whose
    /// root the server does not serve, or one naming a device this server knows
    /// certified by another root, is another person's key, which the entry's
    /// root has no say over: it is dropped.
    pub(crate) async fn take_revocations(
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
        // The roots this server serves: its own person's, and those of the
        // members of its workspaces. A root it serves made a key here, and
        // only a root that made a key has anything to say about it.
        let served = |entry: &RevocationEntry| {
            known
                .iter()
                .any(|cert| cert.user_public_key == entry.user_public_key)
        };
        let entries = entries
            .into_iter()
            .filter(|entry| {
                verify_revocation_entry(entry).is_ok()
                    && served(entry)
                    && !crosses_roots(entry, &known)
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(());
        }
        self.record(&entries)?;
        // Every list of the person's devices reads the device taken back from
        // their own workspace's record.
        for entry in &entries {
            self.mark_revoked(entry).await;
        }
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

    /// Announce `entries`, revocations this device's person's root signed, where devices announce
    /// theirs: this device takes them in itself ([`Self::take_revocations`]), and sends them in
    /// its identity hello on the link of every workspace it holds as a member, whose host takes
    /// them in as it takes any device's ([`Self::take_revocations`] there). From then on the
    /// host refuses whatever the revoked key signs, whichever device sends it.
    ///
    /// # Errors
    /// This device did not take them in, or a link could not carry them.
    pub async fn announce_revocations(&self, entries: Vec<RevocationEntry>) -> Result<(), String> {
        self.take_revocations(entries.clone()).await?;
        let cert = self.device_cert().clone();
        let entries = entries
            .into_iter()
            .filter(|entry| entry.user_public_key == cert.user_public_key)
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(());
        }
        let links = self.member_links();
        let mut carried = Ok(());
        for link in links {
            let hello = neighbour_wire::NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(cert.clone()),
                revocations: entries.clone(),
            };
            if let Err(error) = link.send(hello) {
                carried = Err(error);
            }
        }
        carried
    }

    /// Take in what a refused dial said: a key its person's root retired, as the entry the root
    /// signed. The entry is checked before anything is done with the refusal: the root's
    /// signature first, as every entry the revocation list keeps is checked
    /// ([`verify_revocation_entry`]), then [`Self::take_revocations`]'s own checks of the roots
    /// this device serves and of the roots its workspaces certify. A refusal carrying an entry
    /// that does not verify changes nothing here, whatever the refusing device said.
    pub(crate) async fn take_refusal(&self, refusal: tilo_server_protocol::refusal::Refusal) {
        let tilo_server_protocol::refusal::Refusal::Revoked(entry) = refusal else {
            return;
        };
        if verify_revocation_entry(&entry).is_err() {
            tracing::warn!(
                "a refusal named a revocation its root did not sign; nothing was kept of it"
            );
            return;
        }
        if let Err(error) = self.take_revocations(vec![*entry]).await {
            tracing::warn!(%error, "a refused dial's revocation was not taken in");
        }
    }

    /// Add `entries` to this holder's revocation log and its live list: what
    /// it refuses from each entry's moment on.
    ///
    /// A root signs only about the keys it made, so whose root signed a
    /// verified entry decides whose keys it reaches, and the server keeps them
    /// all: its own person's devices, a member's, and the stand-in approver it
    /// holds for a member whose root lives in one browser tab — a key no
    /// workspace lists, because a stand-in is no device, but the one this host
    /// signs with when it approves a returning sign-in.
    ///
    /// Every entry is kept in the log as long as the server is: a restart
    /// refuses each key it refused before. Only a root the server serves gets
    /// here ([`Self::take_revocations`]), so what the log holds grows with the
    /// revocations of the people it serves and nothing else.
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
        let mut changed = false;
        for entry in &fresh {
            changed |= host
                .revocations
                .write_recover()
                .insert((*entry).clone())
                .map_err(|error| error.to_string())?;
            // The entry holds only the host's own person's devices.
            if entry.user_public_key == host.cert.user_public_key {
                self.inner
                    .mcp_server
                    .forget(&entry.revoked_device_public_key);
            }
            // Where the device was reached while its Tilo was closed goes
            // with its key.
            #[cfg(not(target_arch = "wasm32"))]
            self.forget_push_address_of(&entry.user_public_key, &entry.revoked_device_public_key);
        }
        let fresh = fresh.into_iter().cloned().collect::<Vec<_>>();
        crate::workspace_files::append_revocations(state_root, &fresh)
            .map_err(|error| error.to_string())?;
        if changed {
            crate::authority_epoch::bump();
        }
        Ok(())
    }

    /// Write into `workspace`'s governance every entry of `entries` (and of the
    /// server's live list) it may carry and does not yet: those its
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
        let authority = workspace.record.owner.clone();
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
        // A removal the governance carries without its cut, one recorded
        // before cuts were kept, is completed here by the device that governs
        // the workspace: until then nobody decides what that device wrote.
        if carry.is_empty() && !workspace.peer().governance().lacks_removal_cuts() {
            return Ok(());
        }
        let now = tilo_clock::wall();
        // Written on the workspace as it stands, with its copy locked from the
        // build to the commit, the staging the workspace's other changes use:
        // a write the link takes in meanwhile waits for this commit, which
        // publishes it with the revocation, instead of landing between the
        // copy this change was made on and it and leaving the commit refused
        // for history it was not built on.
        let carried_now = workspace.change(
            key,
            now,
            |_, candidate| {
                let carried = candidate
                    .governance_mut()
                    .publish_governed_revocations(&carry, key, now)?;
                Ok((carried, carried))
            },
            |record, _| Ok(record.collaboration.clone()),
        )?;
        if carried_now {
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

    use super::super::fixtures::{governor, test_host};

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

    /// A device key that is nobody's: distinct per `n`, so entries naming
    /// them are entries about keys no workspace here certifies.
    fn made_up_key(n: u64) -> [u8; 32] {
        let mut key = [0xa5; 32];
        key[..8].copy_from_slice(&n.to_le_bytes());
        key
    }

    /// A refused dial that names a revocation nothing signed is no revocation: the device the
    /// entry names stays in service, nothing is recorded, and no workspace carries it. The
    /// root's own signature is what makes the entry one, as it is for every entry the
    /// revocation list keeps.
    #[tokio::test]
    async fn a_refused_dial_naming_an_unsigned_entry_changes_nothing() {
        // The authority epoch is the whole process's, which every test that narrows anything
        // bumps: only in a process of its own does the epoch stay still of this server's doing.
        if !tilo_test_env::in_own_process_here(
            module_path!(),
            "a_refused_dial_naming_an_unsigned_entry_changes_nothing",
        ) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let created = workspaces.create("Plans").await.unwrap();
        // The person's root, whose key the governor's certificate names: a revocation is the
        // root's to sign, not a device's.
        let root = SigningKey::from_bytes(&[1; 32]);
        assert_eq!(root.verifying_key().to_bytes(), cert.user_public_key);
        let device = SigningKey::from_bytes(&[9; 32]);
        let mut forged = revocation(&root, device.verifying_key().to_bytes());
        forged.signature = [0; 64];
        let epoch = crate::authority_epoch::current();
        workspaces
            .take_refusal(tilo_server_protocol::refusal::Refusal::Revoked(Box::new(
                forged,
            )))
            .await;
        assert!(
            workspaces.revocations().is_empty(),
            "an entry its own root did not sign revokes nothing"
        );
        assert!(
            crate::workspace_files::load_revocations(dir.path())
                .unwrap()
                .is_empty()
        );
        let workspace = workspaces.workspace(&created.workspace).await.unwrap();
        assert!(workspace.lock().await.peer().revocations().is_empty());
        assert_eq!(
            crate::authority_epoch::current(),
            epoch,
            "no session is re-checked"
        );

        // The same entry the root signs is taken, and reaches the workspace.
        let signed = revocation(&root, device.verifying_key().to_bytes());
        workspaces
            .take_refusal(tilo_server_protocol::refusal::Refusal::Revoked(Box::new(
                signed.clone(),
            )))
            .await;
        assert_eq!(
            crate::workspace_files::load_revocations(dir.path()).unwrap(),
            vec![signed.clone()]
        );
        assert!(
            workspaces
                .revocations()
                .is_revoked(&cert.user_public_key, &device.verifying_key().to_bytes())
        );
        assert!(!workspace.lock().await.peer().revocations().is_empty());
        drop(workspace);
        server.shutdown().await;
    }

    /// Bob's root signs an entry naming a device of Ada's: the host keeps
    /// nothing of it, moves no admission and no workspace carries it. Ada's
    /// own root's entry still revokes the device everywhere.
    #[tokio::test]
    async fn another_roots_revocation_of_a_device_is_not_taken() {
        // The authority epoch is the whole process's, which every test that
        // narrows anything bumps: only in a process of its own does the epoch
        // stay still of this server's own doing.
        if !tilo_test_env::in_own_process_here(
            module_path!(),
            "another_roots_revocation_of_a_device_is_not_taken",
        ) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let created = workspaces.create("Plans").await.unwrap();
        let ada = SigningKey::from_bytes(&[1; 32]);
        let mut laptop = cert.clone();
        let laptop_key = SigningKey::from_bytes(&[3; 32]);
        laptop.device_public_key = laptop_key.verifying_key().to_bytes();
        laptop.capabilities = DeviceCapabilities::member();
        sign_device_cert(&mut laptop, &ada).unwrap();
        workspaces
            .enter(&created.workspace, (&laptop, &laptop_key))
            .await
            .expect("Ada's laptop lists itself");
        let workspace = workspaces.workspace(&created.workspace).await.unwrap();

        let epoch = crate::authority_epoch::current();
        let bob = SigningKey::from_bytes(&[7; 32]);
        workspaces
            .take_revocations(vec![revocation(&bob, laptop.device_public_key)])
            .await
            .unwrap();
        assert!(
            crate::workspace_files::load_revocations(dir.path())
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
            crate::workspace_files::load_revocations(dir.path()).unwrap(),
            vec![own.clone()]
        );
        assert!(workspaces.revocations().refuses(&laptop));
        assert_ne!(crate::authority_epoch::current(), epoch);
        assert_eq!(workspace.lock().await.peer().revocations(), vec![own]);
        assert!(
            !workspace
                .lock()
                .await
                .peer()
                .governance()
                .lacks_removal_cuts(),
            "the removal is written with its cut, in the same step"
        );
        drop(workspace);
        server.shutdown().await;
    }

    /// A root the server does not serve announces thousands of entries naming
    /// keys nobody here made. The host keeps none of them, leaves every
    /// session alone, and the log still has room for the revocation a member
    /// announces, which lands and ends that device's session.
    #[tokio::test]
    async fn a_strangers_flood_leaves_room_for_a_members_revocation() {
        // The authority epoch is the whole process's, which every test that
        // narrows anything bumps: only in a process of its own does the epoch
        // stay still of this server's own doing.
        if !tilo_test_env::in_own_process_here(
            module_path!(),
            "a_strangers_flood_leaves_room_for_a_members_revocation",
        ) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let created = workspaces.create("Plans").await.unwrap();
        let ada = SigningKey::from_bytes(&[1; 32]);
        let mut laptop = cert.clone();
        let laptop_key = SigningKey::from_bytes(&[3; 32]);
        laptop.device_public_key = laptop_key.verifying_key().to_bytes();
        laptop.capabilities = DeviceCapabilities::member();
        sign_device_cert(&mut laptop, &ada).unwrap();
        workspaces
            .enter(&created.workspace, (&laptop, &laptop_key))
            .await
            .expect("Ada's laptop lists itself");

        let epoch = crate::authority_epoch::current();
        let stranger = SigningKey::from_bytes(&[9; 32]);
        let flood = (0..5000)
            .map(|n| revocation(&stranger, made_up_key(n)))
            .collect::<Vec<_>>();
        workspaces.take_revocations(flood).await.unwrap();
        assert!(
            crate::workspace_files::load_revocations(dir.path())
                .unwrap()
                .is_empty(),
            "an entry of a root the server does not serve is not kept"
        );
        assert!(workspaces.revocations().is_empty());
        assert_eq!(
            crate::authority_epoch::current(),
            epoch,
            "no session is re-checked"
        );

        let own = revocation(&ada, laptop.device_public_key);
        workspaces
            .take_revocations(vec![own.clone()])
            .await
            .unwrap();
        assert_eq!(
            crate::workspace_files::load_revocations(dir.path()).unwrap(),
            vec![own],
            "the member's revocation lands beside nothing of the stranger's"
        );
        assert!(workspaces.revocations().refuses(&laptop));
        assert_ne!(
            crate::authority_epoch::current(),
            epoch,
            "the member's device is re-checked and its session ends"
        );
        server.shutdown().await;
    }

    /// Ada's root revokes two thousand keys, more than any one write names,
    /// and then her laptop. The log keeps every one of them, so the server
    /// refuses each again once it reads the log after a restart.
    #[tokio::test]
    async fn every_revocation_of_a_served_root_is_kept_across_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let server = test_host(dir.path(), &key, &cert).await;
        let workspaces = server.workspaces();
        let created = workspaces.create("Plans").await.unwrap();
        let ada = SigningKey::from_bytes(&[1; 32]);
        let mut laptop = cert.clone();
        let laptop_key = SigningKey::from_bytes(&[3; 32]);
        laptop.device_public_key = laptop_key.verifying_key().to_bytes();
        laptop.capabilities = DeviceCapabilities::member();
        sign_device_cert(&mut laptop, &ada).unwrap();
        workspaces
            .enter(&created.workspace, (&laptop, &laptop_key))
            .await
            .expect("Ada's laptop lists itself");

        let mut revoked = (0..2000)
            .map(|n| revocation(&ada, made_up_key(n)))
            .collect::<Vec<_>>();
        workspaces.take_revocations(revoked.clone()).await.unwrap();
        let laptops = revocation(&ada, laptop.device_public_key);
        workspaces
            .take_revocations(vec![laptops.clone()])
            .await
            .unwrap();
        revoked.push(laptops);
        server.shutdown().await;

        let mut list = tilo_user_identity::RevocationList::new();
        for entry in crate::workspace_files::load_revocations(dir.path()).unwrap() {
            list.insert(entry).unwrap();
        }
        for entry in &revoked {
            assert!(
                list.is_revoked(&entry.user_public_key, &entry.revoked_device_public_key),
                "every revocation is read back after a restart"
            );
        }
        assert!(list.refuses(&laptop), "the laptop is refused");
    }
}
