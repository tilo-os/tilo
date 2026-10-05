//! Taking over a room another device of this relay's identity governed.
//!
//! Governance belongs to the identity. When its root revokes a room's
//! governing device and signs a [`GovernorSuccession`] naming this relay, the
//! relay becomes the room's governor, whether or not it created the room: it
//! takes the room's state from a replica one of the identity's devices hands
//! it, or from what it retains of the room itself, assumes governance as any
//! successor does, and from then on governs it as one of its own.

use std::sync::Arc;

use tilo_peer_state::succession::GovernorSuccession;
use tilo_peer_state::{Custody, NeighbourMsg, StatePeer};
use tilo_user_identity::{DeviceCert, RevocationList};
use tokio::sync::Mutex as AsyncMutex;
use web_time::SystemTime;

use super::{
    CollaborationError, GovernedRoom, RECORD_VERSION, RoomRecord, RoomSummary, Rooms, admissions,
    collaboration, publication,
};

/// Name a taken-over room keeps when its history names none.
const UNNAMED: &str = "Shared workspace";

impl GovernedRoom {
    /// The room `peer` holds, governed by this relay from now on: `peer` has
    /// assumed governance with this relay's device as its successor.
    fn succeed(
        dir: &std::path::Path,
        mut peer: StatePeer,
        authority: &DeviceCert,
        key: &ed25519_dalek::SigningKey,
        revocations: &Arc<std::sync::RwLock<RevocationList>>,
        now: SystemTime,
    ) -> Result<Self, String> {
        if peer.authority() != authority {
            return Err("the room is not handed to this relay".into());
        }
        let room = peer.room().to_owned();
        let doc = peer.admitted_room().ok_or("room governance missing")?;
        let name = tilo_peer_state::room::extract_governed_fields(doc)
            .0
            .map(|name| name.trim().to_owned())
            .filter(|name| {
                (1..=80).contains(&name.chars().count()) && !name.chars().any(char::is_control)
            })
            .unwrap_or_else(|| UNNAMED.to_owned());
        // Receipts the previous governors signed stay current only until a
        // newer generation replaces them.
        let generation = tilo_peer_state::durability::entries(doc)?
            .values()
            .map(|receipt| receipt.generation)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("room generation exhausted")?;
        super::prepare_governance(&mut peer, key, now)?;
        let outgoing = peer
            .durable_bootstrap_messages(key, now, generation)?
            .iter()
            .map(|message| publication(message, true))
            .collect::<Result<Vec<_>, _>>()?;
        let record = RoomRecord {
            version: RECORD_VERSION,
            workspace_id: crate::room_workspace_id(&room).map_err(|error| error.to_string())?,
            room: room.clone(),
            name,
            authority: authority.clone(),
            peer: peer.durable()?,
            generation,
            outgoing,
            published: 0,
            collaboration: collaboration::CollaborationState::default(),
        };
        let path = dir.join(format!("{room}.room"));
        let this = Self {
            admissions: admissions::Admissions::open(&path)?,
            path,
            stewardship: super::stewardship(dir, &room, authority, key),
            record,
            peer,
            revocations: Arc::clone(revocations),
        };
        this.governing()?;
        this.persist()?;
        Ok(this)
    }
}

impl Rooms {
    /// Check that `device` may hand this relay the room `succession` names:
    /// a current device of the relay's identity other than the governor the
    /// record revokes, a record its root signed naming this relay as
    /// successor, and a room this relay does not govern yet. Nothing about the room's state is read before this holds.
    ///
    /// # Errors
    ///
    /// Returns why the takeover is refused.
    pub(super) async fn check_take_over(
        &self,
        device: &DeviceCert,
        succession: &GovernorSuccession,
    ) -> Result<(), CollaborationError> {
        let now = super::now_ns();
        self.authorize(device, now, "room.take-over")?;
        // The revocation the record carries counts whether or not this relay
        // has learned it yet: the revoked governor hands nothing over.
        if device.device_public_key == succession.previous.device_public_key {
            return Err(CollaborationError::new(
                "the revoked governor cannot hand its room over",
            ));
        }
        if succession.successor != self.inner.host.cert {
            return Err(CollaborationError::new(
                "the succession names another device as successor",
            ));
        }
        succession
            .verify(&succession.room, &succession.previous, now)
            .map_err(CollaborationError::new)?;
        if self.inner.rooms.lock().await.contains_key(&succession.room) {
            return Err(CollaborationError::new(
                "this relay already governs that room",
            ));
        }
        Ok(())
    }

    /// Take over the room `succession` hands this relay. Its state comes from
    /// what this relay retains of the room, replayed through admission;
    /// failing that, from `replica`, the durable replica of the room a device
    /// of the identity holds, pinned to the previous governor or following
    /// exactly this succession. The first that holds the history up to the
    /// record's cutoff, and nothing past it that changes governance, is used. The relay then publishes the room under its own
    /// device, admits its members and governs it like a room it created.
    ///
    /// # Errors
    ///
    /// Returns why the takeover is refused or no source holds the room's
    /// history up to the cutoff.
    pub async fn take_over(
        &self,
        device: &DeviceCert,
        succession: GovernorSuccession,
        replica: Option<Vec<u8>>,
    ) -> Result<RoomSummary, CollaborationError> {
        // One takeover at a time: a room is written to disk before it is
        // listed, and two takeovers of one room must not both write it.
        let _takeover = self.inner.takeovers.lock().await;
        self.check_take_over(device, &succession).await?;
        let host = &self.inner.host;
        let now = SystemTime::now();
        let mut sources = Vec::new();
        let workspace_id = crate::room_workspace_id(&succession.room)
            .map_err(|error| CollaborationError::new(error.to_string()))?;
        if let Ok(retained) = host.records.retained(workspace_id).await {
            sources.push(Custody::Retained(
                retained
                    .into_iter()
                    .filter_map(|retained| {
                        postcard::from_bytes::<NeighbourMsg>(&retained.body)
                            .ok()
                            .map(|message| (retained.author, message))
                    })
                    .collect(),
            ));
        }
        if let Some(bytes) = replica {
            match StatePeer::restore(
                &bytes,
                &succession.room,
                succession.previous.clone(),
                vec![succession.previous.clone()],
                RevocationList::new(),
                now,
            ) {
                Ok(peer) => sources.push(Custody::Replica(Box::new(peer))),
                Err(error) => {
                    tracing::warn!(room = %succession.room, %error, "handed replica does not restore");
                }
            }
        }
        let mut peer =
            StatePeer::take_custody(&succession, sources, now).map_err(CollaborationError::new)?;
        peer.assume_governance(&succession, &host.key, now)
            .map_err(CollaborationError::new)?;
        let room = GovernedRoom::succeed(
            &self.inner.dir,
            peer,
            &host.cert,
            &host.key,
            &host.revocations,
            now,
        )
        .map_err(CollaborationError::new)?;
        if let Err(error) = self.bind(&room).await {
            // An unlisted room must not be governed after a restart either.
            let _ = std::fs::remove_file(&room.path);
            return Err(CollaborationError::new(error));
        }
        let id = room.room().to_owned();
        let room = Arc::new(AsyncMutex::new(room));
        self.inner.rooms.lock().await.insert(id, Arc::clone(&room));
        let mut room = room.lock().await;
        self.recall(&mut room)
            .await
            .map_err(CollaborationError::new)?;
        let settled = match self.settle(&mut room).await {
            Ok(()) => self.carry_revocations(&mut room).await,
            Err(error) => Err(error),
        };
        if let Err(error) = settled {
            self.inner
                .revocations_pending
                .store(true, std::sync::atomic::Ordering::Release);
            return Err(CollaborationError::new(error));
        }
        let summary = room.summary();
        drop(room);
        tracing::info!(
            room = %summary.room,
            workspace_id = summary.workspace_id,
            previous = %hex::encode(succession.previous.device_public_key),
            "governed room taken over"
        );
        Ok(summary)
    }
}
