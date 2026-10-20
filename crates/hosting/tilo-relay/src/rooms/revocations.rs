//! Revocations the relay's rooms carry.
//!
//! A person's root revokes one of their devices, and any device announces
//! the signed entry in a room it shares with the relay (`IdentityHello`).
//! The relay takes it from there. An entry its own person's root signed joins
//! the relay's revocation log, which every handshake, session and
//! administration door reads, so the device is refused everywhere at once.
//! Every room the relay governs writes the entries it may carry into its
//! governance: the device leaves the room's access and channels, and every
//! member's replica refuses it from the room's own history.

use std::time::SystemTime;

use tilo_lock::RwLockRecover as _;
use tilo_user_identity::{RevocationEntry, verify_revocation_entry};

use super::{GovernedRoom, Rooms};

impl Rooms {
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
        let rooms = self
            .inner
            .rooms
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut refused = Ok(());
        for room in rooms {
            let mut room = room.lock().await;
            if let Err(error) = self.carry_revocations(&mut room, &entries).await {
                tracing::warn!(room = %room.room(), %error, "a room did not take a revocation");
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

    /// Write into `room`'s governance every entry of `entries` (and of the
    /// relay's live list) it may carry and does not yet: those its
    /// authority's root signed, and those of a root the room certifies the
    /// revoked device to. A room never carries the revocation of its own
    /// governing device, which would leave it without a writer.
    pub(super) async fn carry_revocations(
        &self,
        room: &mut GovernedRoom,
        entries: &[RevocationEntry],
    ) -> Result<(), String> {
        let key = &self.inner.host.key;
        let authority = room.peer().authority().clone();
        let certified = room.peer().certificates();
        let carried = room.peer().revocations();
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
        let mut candidate = room.peer().clone();
        if candidate
            .governance_mut()
            .publish_governed_revocations(&carry, key, now)?
        {
            room.commit(candidate, key, now, |record, _| {
                Ok(record.collaboration.clone())
            })?;
            tracing::info!(room = %room.room(), revocations = carry.len(), "room carries revocations");
        }
        self.settle(room).await
    }
}
