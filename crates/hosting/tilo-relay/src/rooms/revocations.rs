//! Revocations a governed room carries.
//!
//! The relay keeps every revocation it learns (its own log and the
//! person's recorded revocations) in its live list. As the room's governor
//! it publishes the ones its own identity signed into the room's
//! governance, so every member's replica stops listing the revoked device,
//! not only the relay's admission.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tilo_user_identity::RevocationEntry;
use web_time::SystemTime;

use super::{GovernedRoom, Rooms};

/// How often the governor looks for revocations its rooms do not carry yet.
const OBSERVATION: Duration = Duration::from_secs(1);

/// What identifies one revocation's effect: the revoked device and the
/// earliest time its root revoked it.
type Seen = BTreeSet<([u8; 32], u64)>;

impl Rooms {
    /// The revocations to carry in this relay's rooms: those its own
    /// identity signed, except of the governor itself: a room cannot carry
    /// its own writer's revocation, which would leave it without a writer to
    /// publish it.
    fn governed_revocations(&self) -> Vec<RevocationEntry> {
        let identity = self.inner.host.cert.user_public_key;
        let governor = self.inner.host.cert.device_public_key;
        let mut entries = self
            .revocations()
            .iter()
            .filter(|entry| {
                entry.user_public_key == identity && entry.revoked_device_public_key != governor
            })
            .cloned()
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.revoked_device_public_key);
        entries
    }

    /// Publish into `room` every revocation its governance lacks, as one
    /// governance change, and settle it so the relay stops admitting the
    /// device and the change is retained for every member. Those are the
    /// revocations of [`Self::governed_revocations`], and those a member
    /// identity signed that the room learned from that member's own devices
    /// ([`GovernedRoom::follow`]). A change committed earlier but not yet
    /// published is settled too. A room whose governor is revoked takes
    /// nothing.
    pub(super) async fn carry_revocations(&self, room: &mut GovernedRoom) -> Result<(), String> {
        if room.governing().is_err() {
            return Ok(());
        }
        let key = &self.inner.host.key;
        let now = SystemTime::now();
        let authority = room.peer().authority().user_public_key;
        let members = room.peer().member_identities(now)?;
        let certified = room.peer().certificates();
        let carried = room.peer().governance_revocations();
        let mut entries = self.governed_revocations();
        entries.extend(room.peer().revocations().into_iter().filter(|entry| {
            entry.user_public_key != authority
                && members.binary_search(&entry.user_public_key).is_ok()
        }));
        // The authority's identity revokes any of its keys; a member only
        // its own devices the room certifies. Neither revokes a key another
        // identity certified in the room.
        entries.retain(|entry| {
            let target = certified
                .iter()
                .find(|cert| cert.device_public_key == entry.revoked_device_public_key);
            match target {
                Some(cert) => cert.user_public_key == entry.user_public_key,
                None => entry.user_public_key == authority,
            }
        });
        entries.retain(|entry| {
            !carried.iter().any(|known| {
                known.user_public_key == entry.user_public_key
                    && known.revoked_device_public_key == entry.revoked_device_public_key
                    && known.revoked_at_ns <= entry.revoked_at_ns
            })
        });
        // One identity's batch at a time: an identity whose entries cannot
        // be published does not hold back another's, the authority's first.
        let mut roots = entries
            .iter()
            .map(|entry| entry.user_public_key)
            .collect::<Vec<_>>();
        roots.sort_by_key(|root| (*root != authority, *root));
        roots.dedup();
        let mut refused = None;
        for root in roots {
            let batch = entries
                .iter()
                .filter(|entry| entry.user_public_key == root)
                .cloned()
                .collect::<Vec<_>>();
            let mut candidate = room.peer().clone();
            let published = match candidate.publish_governed_revocations(&batch, key, now) {
                Ok(published) => published,
                Err(error) => {
                    refused.get_or_insert(error);
                    continue;
                }
            };
            if published {
                room.commit(candidate, key, now, |record, _| {
                    Ok(record.collaboration.clone())
                })?;
                tracing::info!(
                    room = %room.room(),
                    identity = %hex::encode(root),
                    revocations = batch.len(),
                    "governed room carries new revocations"
                );
            }
        }
        if room.unpublished() {
            self.settle(room).await?;
        }
        refused.map_or(Ok(()), Err)
    }

    /// Carry new revocations into every room as the relay learns them. A
    /// room that could not take them is retried on the next observation, as
    /// is every room after one could not be created whole.
    pub(super) fn watch_revocations(&self) {
        let rooms = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(OBSERVATION);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut carried = Seen::new();
            loop {
                tick.tick().await;
                let Some(inner) = rooms.upgrade() else { break };
                let this = Self { inner };
                let seen = this
                    .governed_revocations()
                    .iter()
                    .map(|entry| (entry.revoked_device_public_key, entry.revoked_at_ns))
                    .collect::<Seen>();
                let retry = this.inner.revocations_pending.swap(false, Ordering::AcqRel);
                if seen == carried && !retry {
                    continue;
                }
                let governed = this
                    .inner
                    .rooms
                    .lock()
                    .await
                    .values()
                    .cloned()
                    .collect::<Vec<_>>();
                let mut complete = true;
                for room in governed {
                    let mut room = room.lock().await;
                    if let Err(error) = this.carry_revocations(&mut room).await {
                        complete = false;
                        tracing::warn!(room = %room.room(), %error, "governed room did not take a revocation");
                    }
                }
                if complete {
                    carried = seen;
                } else {
                    this.inner
                        .revocations_pending
                        .store(true, Ordering::Release);
                }
            }
        });
    }
}
