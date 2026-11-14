//! Revocations the relay's workspaces carry.
//!
//! A person's root revokes a key it made, and any device of theirs announces
//! the signed entry in a workspace it shares with the relay (`IdentityHello`).
//! The relay takes it from there. An entry reaches only keys its own root
//! certified: one naming a device the relay knows certified by another root
//! is dropped, and every list the relay keeps asks a revocation of the root
//! that certified the key. An entry speaks for a root the relay serves, too:
//! a root of its own person or of a member of one of its workspaces. Another
//! root made no key here, so what it announces is dropped unread, and nobody
//! can fill the relay's log with entries that reach nothing. Every entry the
//! relay keeps joins its revocation log, which every handshake, session and
//! administration door reads, so the key is refused everywhere at once — a
//! device of the person's, and the stand-in approver this host holds for
//! them, which no workspace lists because a stand-in is no device. Every
//! workspace the relay governs also writes the entries it may carry into its
//! governance: the device leaves the workspace's access and channels, and
//! every member's replica refuses it from the workspace's own history.

use tilo_lock::RwLockRecover as _;
use tilo_user_identity::{DeviceCert, RevocationEntry, verify_revocation_entry};

use super::{GovernedWorkspace, Workspaces};

impl Workspaces {
    /// Take in the root-signed `entries` a device announced. An entry whose
    /// root the relay does not serve, or one naming a device this relay knows
    /// certified by another root, is another person's key, which the entry's
    /// root has no say over: it is dropped.
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
        // The roots this relay serves: its own person's, and those of the
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
    ///
    /// Every entry is kept in the log as long as the relay is: a restart
    /// refuses each key it refused before. Only a root the relay serves gets
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
            // The door holds only the host's own person's devices.
            if entry.user_public_key == host.cert.user_public_key {
                self.inner
                    .agent_door
                    .forget(&entry.revoked_device_public_key);
            }
        }
        let fresh = fresh.into_iter().cloned().collect::<Vec<_>>();
        crate::store::append_revocations(state_root, &fresh).map_err(|error| error.to_string())?;
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
        let now = tilo_clock::wall();
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
    use tilo_relay_protocol::WorkspaceId;
    use tilo_user_identity::{
        DeviceCapabilities, RevocationEntry, RevocationReason, sign_device_cert,
        sign_revocation_entry,
    };

    use super::super::fixtures::{admit_view_reader, door_join, governor, test_host};

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

    /// Bob's root signs an entry naming a device of Ada's: the host keeps
    /// nothing of it, moves no admission and no workspace carries it. Ada's
    /// own root's entry still revokes the device everywhere.
    #[tokio::test]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the relay a test binds is kept until it ends; dropping it earlier closes the host the test drives"
    )]
    async fn another_roots_revocation_of_a_device_is_not_taken() {
        // The authority epoch is the whole process's, which every test that
        // narrows anything bumps: only in a process of its own is the epoch
        // standing still this relay's own doing.
        if !tilo_test_env::in_own_process_here(
            module_path!(),
            "another_roots_revocation_of_a_device_is_not_taken",
        ) {
            return;
        }
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

    /// A root the relay does not serve announces thousands of entries naming
    /// keys nobody here made. The host keeps none of them, leaves every
    /// session alone, and the log still has room for the revocation a member
    /// announces, which lands and ends that device's session.
    #[tokio::test]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the relay a test binds is kept until it ends; dropping it earlier closes the host the test drives"
    )]
    async fn a_strangers_flood_leaves_room_for_a_members_revocation() {
        // The authority epoch is the whole process's, which every test that
        // narrows anything bumps: only in a process of its own is the epoch
        // standing still this relay's own doing.
        if !tilo_test_env::in_own_process_here(
            module_path!(),
            "a_strangers_flood_leaves_room_for_a_members_revocation",
        ) {
            return;
        }
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

        let epoch = crate::authority_epoch::current();
        let stranger = SigningKey::from_bytes(&[9; 32]);
        let flood = (0..5000)
            .map(|n| revocation(&stranger, made_up_key(n)))
            .collect::<Vec<_>>();
        workspaces.take_revocations(flood).await.unwrap();
        assert!(
            crate::store::load_revocations(dir.path())
                .unwrap()
                .is_empty(),
            "an entry of a root the relay does not serve is not kept"
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
            crate::store::load_revocations(dir.path()).unwrap(),
            vec![own],
            "the member's revocation lands beside nothing of the stranger's"
        );
        assert!(workspaces.revocations().refuses(&laptop));
        assert_ne!(
            crate::authority_epoch::current(),
            epoch,
            "the member's device is re-checked and its session ends"
        );
        relay.shutdown().await;
    }

    /// Ada's root revokes thousands of keys, more than the log once had room
    /// for, and then her laptop. The log keeps every one of them, so the host
    /// refuses each again once it reads the log after a restart.
    #[tokio::test]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the relay a test binds is kept until it ends; dropping it earlier closes the host the test drives"
    )]
    async fn every_revocation_of_a_served_root_is_kept_across_a_restart() {
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

        let mut revoked = (0..5000)
            .map(|n| revocation(&ada, made_up_key(n)))
            .collect::<Vec<_>>();
        workspaces.take_revocations(revoked.clone()).await.unwrap();
        let laptops = revocation(&ada, laptop.device_public_key);
        workspaces
            .take_revocations(vec![laptops.clone()])
            .await
            .unwrap();
        revoked.push(laptops);
        relay.shutdown().await;

        let mut list = tilo_user_identity::RevocationList::new();
        for entry in crate::store::load_revocations(dir.path()).unwrap() {
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

    /// Ada's phone announces, in its identity hello, that her root revoked
    /// her laptop: a device this workspace never listed, so its governance
    /// carries no row to keep the revocation on, and the phone's hello is
    /// the only record of it the workspace's replay holds. Ada is then
    /// removed, and her phone with her. The host forgets the hellos of
    /// devices it no longer admits, but not this one: a newcomer still
    /// learns the revocation, and refuses the laptop.
    #[tokio::test]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the relay a test binds is kept until it ends; dropping it earlier closes the host the test drives"
    )]
    async fn a_hello_that_alone_brings_a_revocation_outlives_its_device() {
        use crate::workspaces::RemoveMemberRequest;
        use tilo_peer_state::join::JoinTransportResponse;

        let dir = tempfile::tempdir().unwrap();
        let (key, cert) = governor();
        let relay = test_host(dir.path(), &key, &cert).await;
        let workspaces = relay.workspaces();
        let created = workspaces.create("Plans").await.unwrap();
        let ada = SigningKey::from_bytes(&[21; 32]);
        let device = |seed: u8, root: &SigningKey| {
            let key = SigningKey::from_bytes(&[seed; 32]);
            let mut device = cert.clone();
            device.user_public_key = root.verifying_key().to_bytes();
            device.device_public_key = key.verifying_key().to_bytes();
            device.capabilities = DeviceCapabilities::member();
            device.issuer = None;
            sign_device_cert(&mut device, root).unwrap();
            (key, device)
        };
        let (phone_key, phone) = device(23, &ada);
        let (_, laptop) = device(24, &ada);
        door_join(
            workspaces,
            &cert,
            &created.workspace,
            (&phone_key, &phone),
            11,
        )
        .await;

        // The phone's hello, retained as a member's is, and taken in as the
        // host takes in a member's notice.
        let revoked = revocation(&ada, laptop.device_public_key);
        let hosted = retain_hello(workspaces, created.workspace_id, &phone, &revoked).await;
        let channel = crate::store::compaction::hello_channel(&phone.device_public_key);
        workspaces
            .take_revocations(vec![revoked.clone()])
            .await
            .unwrap();
        let governed = workspaces.workspace(&created.workspace).await.unwrap();
        assert!(
            !governed
                .lock()
                .await
                .peer()
                .revocations()
                .contains(&revoked),
            "no row of the workspace's governance carries the laptop's revocation"
        );

        workspaces
            .remove_member(
                &created.workspace,
                &cert,
                RemoveMemberRequest {
                    request_id: [12; 32],
                    user_public_key: ada.verifying_key().to_bytes(),
                },
            )
            .await
            .unwrap();
        let retained =
            crate::compaction::owner_run(&hosted, move |store| store.channel_entries(&channel))
                .await
                .unwrap();
        assert_eq!(
            retained.len(),
            1,
            "the removed phone's hello is kept for the revocation it alone brings"
        );

        // A newcomer builds its replica from its join's answer and the
        // workspace's replay, which carries the phone's hello.
        let bo = SigningKey::from_bytes(&[25; 32]);
        let (tablet_key, tablet) = device(26, &bo);
        let (_, answer) = door_join(
            workspaces,
            &cert,
            &created.workspace,
            (&tablet_key, &tablet),
            13,
        )
        .await;
        let JoinTransportResponse::Complete(joined) = answer else {
            panic!("the newcomer is admitted: {answer:?}");
        };
        assert!(
            !joined.revocations.contains(&revoked),
            "the join's answer does not bring it: the hello is its only carrier"
        );
        let replica = replica_from(&created.workspace, &cert, &joined, &retained);
        let known = replica.revocations();
        assert!(
            known.contains(&revoked),
            "the newcomer learns the laptop's revocation"
        );
        let mut list = tilo_user_identity::RevocationList::new();
        for entry in known {
            list.insert(entry).unwrap();
        }
        assert!(list.refuses(&laptop), "and refuses the laptop");
        drop(governed);
        relay.shutdown().await;
    }

    /// Retain `device`'s identity hello bringing `revoked` in the workspace
    /// `workspace_id`, as the host retains a member's. Answers the workspace
    /// as the host holds it.
    async fn retain_hello(
        workspaces: &super::Workspaces,
        workspace_id: WorkspaceId,
        device: &tilo_user_identity::DeviceCert,
        revoked: &RevocationEntry,
    ) -> crate::workspace::SharedWorkspace {
        let hello = neighbour_wire::NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(device.clone()),
            revocations: vec![revoked.clone()],
        };
        let channel = crate::store::compaction::hello_channel(&device.device_public_key);
        let frame = crate::transport::encode(&crate::transport::Envelope::Delta {
            workspace_id,
            author_device_public_key: device.device_public_key,
            authored_at_ns: tilo_clock::unix_now_ns(),
            delta: crate::coedit::CoeditPayload::Checkpoint {
                channel,
                body: postcard::to_stdvec(&hello).unwrap(),
            }
            .encode()
            .unwrap(),
            signature: [0; 64],
        })
        .unwrap();
        let hosted = workspaces
            .inner
            .host
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned()
            .unwrap();
        crate::compaction::owner_run(&hosted, move |store| {
            store.retain_checkpoint(&channel, &frame)
        })
        .await
        .unwrap();
        hosted
    }

    /// The replica a device builds from its join's answer `joined` and the
    /// `retained` entries its replay brings.
    fn replica_from(
        workspace: &str,
        authority: &tilo_user_identity::DeviceCert,
        joined: &tilo_peer_state::join::JoinBootstrap,
        retained: &[crate::store::compaction::ChannelEntry],
    ) -> tilo_peer_state::StatePeer {
        let now = tilo_clock::wall();
        let mut replica = tilo_peer_state::StatePeer::from_join(
            workspace,
            authority.clone(),
            joined.certified_roster.clone(),
            &joined.revocations,
            now,
        )
        .unwrap();
        for entry in retained {
            let message = postcard::from_bytes(&entry.body).unwrap();
            replica
                .receive_from(
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(entry.author),
                    message,
                    now,
                )
                .unwrap();
        }
        replica
    }
}
