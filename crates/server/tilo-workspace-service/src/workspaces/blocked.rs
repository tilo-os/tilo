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

use std::collections::BTreeSet;

use tilo_lock::RwLockRecover as _;
use tilo_resource::{Collection, Id, Model, Refusal, resource};

use super::person_workspace;
use super::Workspaces;

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

/// The keys this host refuses now, as a record says them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockedKeys {
    devices: BTreeSet<[u8; 32]>,
    users: BTreeSet<[u8; 32]>,
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
        }
    }

    /// Whether `device`, a device of `user`, is blocked.
    #[must_use]
    pub(crate) fn blocks(&self, device: &[u8; 32], user: &[u8; 32]) -> bool {
        self.devices.contains(device) || self.users.contains(user)
    }
}

/// `text` as 32 bytes in lowercase hex, or `None`.
pub(crate) fn key_bytes(text: &str) -> Option<[u8; 32]> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(text, &mut bytes)
        .ok()
        .filter(|()| text == hex::encode(bytes))?;
    Some(bytes)
}

impl Workspaces {
    /// The keys this host's operator blocks now ([`BlockedKeys`]).
    #[must_use]
    pub(crate) fn blocked_keys(&self) -> BlockedKeys {
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

    /// Read the blocked-keys record again and keep what it says. A host that
    /// does not hold its operator's workspace keeps the keys it had: nothing
    /// says they were lifted.
    pub(crate) async fn refresh_blocked(&self) {
        let Ok(state) = person_workspace::read::<Blocked>(self, BLOCKED).await else {
            return;
        };
        self.keep_blocked(BlockedKeys::of(&state));
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

    /// Follow the blocked-keys record: read it once as the host opens, then
    /// again whenever this host takes a write of it and whenever the operator's
    /// workspace settles.
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
                        // The record names the keys; a change to any other says
                        // nothing about them.
                        Ok(write) if write.workspace == admin && write.app == BLOCKED.0 && write.record == BLOCKED.1 => {}
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
