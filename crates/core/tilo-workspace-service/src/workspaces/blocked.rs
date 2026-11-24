//! The keys a host's operator blocks: a record of their own workspace, refused
//! everywhere this host serves.
//!
//! A host is a device of the person who runs it, and that person keeps what is
//! theirs in their own workspace ([`super::person_workspace`]). The blocked
//! keys are one record of it ([`BLOCKED`]): the device public keys and the user
//! root public keys the operator names, each a typed action on the record
//! ([`Blocked`]), so a block has an author and a history and reaches the
//! operator's other devices as any record of theirs does instead of living in a
//! file beside them. Every host that holds the record refuses those keys where
//! the record stands: once a workspace's own governance decides membership, the
//! operator's two levers are stopping a workspace and this explicit block, and
//! no workspace's membership or entry rule lifts a block.
//!
//! The record is read through the store as the host serves it, and kept in
//! [`BlockedKeys`] so an admission decision reads it without waiting: the
//! follower started with the host reads it again whenever this host takes a
//! write of it and whenever the operator's workspace settles, and every change
//! announces a narrowed authority ([`crate::authority_epoch::bump`]), which
//! re-checks the live links and sessions. A blocked device's links and sessions
//! end on that check, as a revoked device's do, and it is refused when it dials
//! again until the operator unblocks it.
//!
//! A device the person took back is refused the same way: the record of their
//! devices ([`DEVICES`]) holds the root-signed revocation of it, and every host
//! that holds that record reads the entry with the blocked keys and refuses the
//! key as revoked once the change arrives, on every protocol, until its
//! certificate ends.

use std::collections::BTreeSet;

use tilo_lock::RwLockRecover as _;
use tilo_resource::{Collection, Id, Model, Refusal, resource};
use tilo_user_identity::RevocationEntry;

use super::Workspaces;
use super::person_workspace;
use super::person_workspace::devices::{DEVICES, Devices};

/// The record of the operator's blocked keys in their own workspace.
pub const BLOCKED: (&str, &str) = ("so.tilo.host", "blocked");

/// The person's blocked keys: devices by their key, and people by their root.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Blocked {
    /// Device public keys, lowercase hex.
    pub devices: Collection<BlockedKey>,
    /// User root public keys, lowercase hex: every device of that person.
    pub users: Collection<BlockedKey>,
}

/// One blocked key. It holds nothing of its own: the key is the entry's id, and
/// the record's history says who blocked it and when.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct BlockedKey {}

#[resource(kind = "tilo.host.blocked", read_content = tilo_resource::read::fields)]
impl Blocked {
    /// Block the device `device` (32 bytes, lowercase hex) on every host that
    /// holds this record.
    #[action(external)]
    pub fn block_device(&mut self, device: String) -> Result<(), Refusal> {
        let key = key_id(&device)?;
        if !self.devices.contains(&key) {
            self.devices.push(key, BlockedKey::default());
        }
        Ok(())
    }

    /// Stop blocking the device `device`.
    #[action(external)]
    pub fn unblock_device(&mut self, device: String) -> Result<(), Refusal> {
        self.devices.remove(&key_id(&device)?);
        Ok(())
    }

    /// Block the person whose root is `user` (32 bytes, lowercase hex): every
    /// device of theirs is refused on every host that holds this record.
    #[action(external)]
    pub fn block_user(&mut self, user: String) -> Result<(), Refusal> {
        let key = key_id(&user)?;
        if !self.users.contains(&key) {
            self.users.push(key, BlockedKey::default());
        }
        Ok(())
    }

    /// Stop blocking the person whose root is `user`.
    #[action(external)]
    pub fn unblock_user(&mut self, user: String) -> Result<(), Refusal> {
        self.users.remove(&key_id(&user)?);
        Ok(())
    }
}

/// `text` as a key's id: 32 bytes in lowercase hex, the spelling the record
/// keeps and every reader compares.
fn key_id(text: &str) -> Result<Id, Refusal> {
    let text = text.trim();
    let mut bytes = [0; 32];
    hex::decode_to_slice(text, &mut bytes)
        .ok()
        .filter(|()| text == hex::encode(bytes))
        .map(|()| Id::from_raw(text.to_owned()))
        .ok_or_else(|| Refusal::new("a key is 32 bytes in lowercase hex"))
}

/// The keys this host refuses now, as the records say them: those the operator
/// blocks, and the devices of theirs the record of their devices holds a
/// revocation for. It holds what that record certifies too, so a host asked
/// whether a key is one of the person's answers from here ([`crate::sources`] is
/// filled from it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockedKeys {
    devices: BTreeSet<[u8; 32]>,
    users: BTreeSet<[u8; 32]>,
    certified: BTreeSet<[u8; 32]>,
    /// The root-signed revocations the record of the person's devices holds, each checked
    /// against the root that certified the device it names.
    revocations: Vec<RevocationEntry>,
    /// The keys the record's entries confine to one workspace alone, with its id: their
    /// certificates, which the root signed, say so.
    confined: std::collections::BTreeMap<[u8; 32], [u8; 16]>,
}

impl BlockedKeys {
    /// What `state` holds: keys the actions wrote, and nothing of an entry
    /// written outside them that does not spell one.
    #[must_use]
    pub(crate) fn of(state: &Blocked) -> Self {
        let keys = |collection: &Collection<BlockedKey>| -> BTreeSet<[u8; 32]> {
            collection
                .ids()
                .filter_map(|id| key_bytes(id.as_str()))
                .collect()
        };
        Self {
            devices: keys(&state.devices),
            users: keys(&state.users),
            certified: BTreeSet::new(),
            revocations: Vec::new(),
            confined: std::collections::BTreeMap::new(),
        }
    }

    /// What the record of `person`'s devices says: the keys their root certified, and the entries
    /// it signed for them. Nothing blocked: each key is the one its root certified, and an entry
    /// another root signed counts for nothing.
    #[must_use]
    pub(crate) fn revoking(record: &Devices, person: &[u8; 32]) -> Self {
        let listed = record.listed(person);
        Self {
            certified: listed
                .iter()
                .map(|listed| listed.cert.device_public_key)
                .collect(),
            revocations: record.revocations(person),
            confined: listed
                .iter()
                .filter_map(|listed| {
                    Some((
                        listed.cert.device_public_key,
                        listed.cert.capabilities.workspace?,
                    ))
                })
                .collect(),
            ..Self::default()
        }
    }

    /// The keys the record of the person's devices confines to one workspace alone, each with
    /// that workspace's id.
    pub fn confined_keys(&self) -> impl Iterator<Item = ([u8; 32], [u8; 16])> + '_ {
        self.confined
            .iter()
            .map(|(key, workspace)| (*key, *workspace))
    }

    /// The keys the record of the person's devices holds, as their root certified them.
    pub fn certified_keys(&self) -> impl Iterator<Item = &[u8; 32]> {
        self.certified.iter()
    }

    /// The revocations the record of the person's devices holds, each the root's own signature.
    #[must_use]
    pub fn revocations(&self) -> &[RevocationEntry] {
        &self.revocations
    }

    /// Whether the root of the person's devices took `device` back.
    #[must_use]
    pub fn revokes_key(&self, device: &[u8; 32]) -> bool {
        self.revocations
            .iter()
            .any(|entry| entry.revoked_device_public_key == *device)
    }

    /// Whether the record of the person's devices holds `device`.
    #[must_use]
    pub fn certified(&self, device: &[u8; 32]) -> bool {
        self.certified.contains(device)
    }

    /// A number that moves when what this holds does: what a cache of the person's devices is
    /// keyed on.
    #[must_use]
    pub fn generation(&self) -> u64 {
        let mut generation = (self.devices.len() as u64)
            ^ ((self.users.len() as u64) << 8)
            ^ ((self.certified.len() as u64) << 16)
            ^ ((self.revocations.len() as u64) << 24)
            ^ ((self.confined.len() as u64) << 32);
        for key in self.certified.iter().chain(
            self.revocations
                .iter()
                .map(|entry| &entry.revoked_device_public_key),
        ) {
            let head = u64::from_le_bytes(
                key.get(..8)
                    .and_then(|head| head.try_into().ok())
                    .unwrap_or_default(),
            );
            generation = generation.rotate_left(1) ^ head;
        }
        generation
    }

    /// Whether `device`, a device of `user`, is blocked.
    #[must_use]
    pub(crate) fn blocks(&self, device: &[u8; 32], user: &[u8; 32]) -> bool {
        self.devices.contains(device) || self.users.contains(user)
    }

    /// Whether the record of the person's devices holds a revocation for `cert`: its own key, or
    /// the grant key that minted it, since a minted key is that grant narrowed and never outlives
    /// it.
    #[must_use]
    pub fn revokes(&self, cert: &tilo_user_identity::DeviceCert) -> bool {
        self.revokes_key(&cert.device_public_key)
            || cert.issuer.as_ref().is_some_and(|issuer| {
                issuer.capabilities.agent.is_some() && self.revokes_key(&issuer.device_public_key)
            })
    }
}

/// `text` as 32 bytes in lowercase hex, or `None`.
pub(crate) fn key_bytes(text: &str) -> Option<[u8; 32]> {
    let mut bytes = [0; 32];
    tilo_failure::ignored!(
        "text that is not a key's hex names no key, which is the answer asked for",
        hex::decode_to_slice(text, &mut bytes).ok()
    )
    .filter(|()| text == hex::encode(bytes))?;
    Some(bytes)
}

impl Workspaces {
    /// The keys this host's operator blocks and the person's devices record holds now
    /// ([`BlockedKeys`]).
    #[must_use]
    pub fn blocked_keys(&self) -> BlockedKeys {
        self.inner.host.blocked.read_recover().clone()
    }

    /// Apply `action` to the operator's blocked-keys record as this host, and
    /// keep what it says: the one write a block or an unblock makes.
    ///
    /// # Errors
    /// This host holds no copy of its operator's workspace, the record refused
    /// the action, or it could not be written.
    pub(crate) async fn apply_blocked(
        &self,
        action: tilo_resource::Invoke<Blocked>,
    ) -> Result<(), String> {
        person_workspace::apply::<Blocked>(self, BLOCKED, action).await?;
        self.refresh_blocked().await;
        // A block narrows what admission reads even when the record already
        // said so on another device: the announcement is what ends the links
        // and sessions of a key it now refuses.
        crate::authority_epoch::bump();
        Ok(())
    }

    /// Read the blocked-keys record and the record of the operator's devices
    /// again, and keep what they say. A record this host holds no copy of
    /// leaves the keys it had: nothing says they were lifted.
    pub(crate) async fn refresh_blocked(&self) {
        let blocked = person_workspace::read::<Blocked>(self, BLOCKED).await;
        let devices = person_workspace::read::<Devices>(self, DEVICES).await;
        let mut next = self.blocked_keys();
        if let Ok(state) = blocked {
            let of = BlockedKeys::of(&state);
            (next.devices, next.users) = (of.devices, of.users);
        }
        if let Ok(record) = devices {
            let of = BlockedKeys::revoking(&record, &self.device_cert().user_public_key);
            (next.certified, next.revocations, next.confined) =
                (of.certified, of.revocations, of.confined);
        }
        self.keep_blocked(next);
    }

    /// Keep `next` as the keys this host refuses, announcing the narrowing that
    /// changes what admission decides.
    fn keep_blocked(&self, next: BlockedKeys) {
        let changed = {
            let mut keys = self.inner.host.blocked.write_recover();
            if *keys == next {
                false
            } else {
                *keys = next;
                true
            }
        };
        if changed {
            crate::authority_epoch::bump();
        }
    }

    /// Follow the blocked-keys record and the record of the operator's devices:
    /// read them once as the host opens, then again whenever this host takes a
    /// write of either and whenever the operator's workspace settles.
    pub(super) fn follow_blocked(&self) {
        let workspaces = self.downgrade();
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let admin = person_workspace::workspace_id(self);
        let mut writes = self.document_writes();
        let mut governance = self.governance_changes();
        runtime.spawn(async move {
            loop {
                let Some(workspaces) = workspaces.upgrade() else {
                    return;
                };
                workspaces.refresh_blocked().await;
                drop(workspaces);
                tokio::select! {
                    write = writes.recv() => match write {
                        // The records name the keys; a change to any other says
                        // nothing about them.
                        Ok(write) if write.workspace == admin
                            && [BLOCKED, DEVICES].iter().any(|(app, record)| {
                                write.app == *app && write.record == *record
                            }) => {}
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    },
                    change = governance.recv() => match change {
                        Ok(workspace) if workspace == admin => {}
                        Ok(_) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    },
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, RevocationReason, sign_device_cert, sign_revocation_entry,
    };

    /// The person's devices record, holding the VM their root confined to one workspace, says
    /// so to every host that reads it, and says it is taken back once their root revokes it.
    #[test]
    fn the_devices_record_says_a_vm_reaches_its_workspace_alone_until_it_is_revoked() {
        let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
        let person = root.verifying_key().to_bytes();
        let workspace = [0x11; 16];
        let mut vm = DeviceCert {
            user_public_key: person,
            device_public_key: [9; 32],
            device_label: "Workspace computer".into(),
            capabilities: DeviceCapabilities::workspace_device(workspace),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut vm, &root).unwrap();
        let mut record = tilo_resource::Replica::<Devices>::new();
        record.apply(Devices::keeping(&vm).unwrap()).unwrap();
        let read = BlockedKeys::revoking(&record.state().unwrap(), &person);
        assert!(read.certified(&vm.device_public_key));
        assert_eq!(
            read.confined_keys().collect::<Vec<_>>(),
            vec![(vm.device_public_key, workspace)]
        );
        assert!(!read.revokes(&vm));

        let mut entry = RevocationEntry {
            user_public_key: person,
            revoked_device_public_key: vm.device_public_key,
            revoked_at_ns: 2,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root).unwrap();
        record
            .apply(Devices::REVOKE_DEVICE.with((
                hex::encode(vm.device_public_key),
                hex::encode(postcard::to_allocvec(&entry).unwrap()),
            )))
            .unwrap();
        let read = BlockedKeys::revoking(&record.state().unwrap(), &person);
        assert!(read.revokes(&vm), "the person's root took the VM back");
    }
}
