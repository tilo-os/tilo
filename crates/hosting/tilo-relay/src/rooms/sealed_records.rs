//! The governor's own reads and writes of records its members write sealed.
//!
//! In a room whose channels this relay stewards, members seal every write
//! of a record, and the governor's copy keeps those writes as the record's
//! ciphertext history ([`tilo_peer_state::StatePeer::is_sealed_record`]). As the channels'
//! steward the relay holds their content keys: it reads such a record by
//! opening that history over the record's cleartext base, writes to it
//! sealed like any member does, and compacts the history before it reaches
//! its bound, so its copy goes on following the record however long the
//! room keeps writing.

use channel_crypto::ChannelContentKeys;
use collab_types::{ArtifactId, ContentRetention, EncryptionMetadata, SignedOp};
#[cfg(feature = "agents")]
use ed25519_dalek::SigningKey;
#[cfg(feature = "agents")]
use neighbour_wire::NeighbourMsg;
#[cfg(feature = "agents")]
use tilo_peer_state::StatePeer;

use super::{GovernedRoom, Rooms, SystemTime};

/// Member writes followed before a failed compaction is tried again. A
/// record whose merged snapshot outgrew one mutation never compacts; trying
/// on every write opened and merged its whole history each time.
const COMPACTION_RETRY: usize = 256;

/// The content keys of one record's channel, as its steward holds them.
pub(super) struct RecordKeys {
    keys: ChannelContentKeys,
    artifact: ArtifactId,
}

impl RecordKeys {
    /// Open any write of the record's history, under any epoch the keys
    /// hold, a compaction included.
    pub(super) fn open(&self) -> impl FnMut(&SignedOp) -> Result<Vec<u8>, String> + '_ {
        |op| {
            let metadata = op.encryption.ok_or("the write is not sealed")?;
            let ciphertext =
                collab_types::sealed_ciphertext(op).map_err(|error| error.to_string())?;
            self.keys
                .open_content(op.artifact, &metadata, &ciphertext)
                .map(|plaintext| plaintext.to_vec())
                .map_err(|error| error.to_string())
        }
    }

    /// Seal content of the record under the channel's current epoch.
    fn seal(
        &self,
        retention: ContentRetention,
    ) -> impl FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> + '_ {
        move |plaintext| {
            self.keys
                .seal_content(self.artifact, retention, plaintext)
                .map_err(|error| error.to_string())
        }
    }
}

impl GovernedRoom {
    /// The keys of `record` of `app`'s channel when the record is sealed:
    /// `None` for a record written in the clear.
    ///
    /// # Errors
    ///
    /// The record is sealed and this relay does not steward its channel,
    /// or the channel's keys cannot be read.
    pub(super) async fn record_keys(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<RecordKeys>, String> {
        if !self.peer.is_sealed_record(app, record) {
            return Ok(None);
        }
        let steward = self
            .stewardship
            .as_ref()
            .ok_or("the record is sealed and this relay does not steward its channel")?;
        let room = self.room();
        let channel = collab_types::document_channel_id(app, room, record);
        let keys = steward.channels.lock().await.existing_keys(channel)?;
        Ok(Some(RecordKeys {
            keys,
            artifact: collab_types::document_artifact_id(app, room, record),
        }))
    }

    /// The plaintext of `op`, a member's sealed write of `record` of `app`,
    /// opened with its channel's keys: `None` when this relay does not
    /// steward the room's channels or holds no keys for this one yet.
    ///
    /// # Errors
    ///
    /// The keys cannot be read, or the write does not open with them.
    pub(super) async fn open_write(
        &self,
        app: &str,
        record: &str,
        op: &SignedOp,
    ) -> Result<Option<Vec<u8>>, String> {
        let Some(steward) = self.stewardship.as_ref() else {
            return Ok(None);
        };
        let room = self.room();
        let channel = collab_types::document_channel_id(app, room, record);
        let Ok(keys) = steward.channels.lock().await.existing_keys(channel) else {
            return Ok(None);
        };
        let keys = RecordKeys {
            keys,
            artifact: collab_types::document_artifact_id(app, room, record),
        };
        let mut open = keys.open();
        open(op).map(Some)
    }

    /// `record` of `app` as the governor reads it, saved: the record itself,
    /// or for a sealed one its history opened with the channel's keys.
    ///
    /// # Errors
    ///
    /// As [`Self::record_keys`], or a sealed write does not open or merge.
    pub(super) async fn readable_document(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<Vec<u8>>, String> {
        match self.record_keys(app, record).await? {
            None => Ok(self.peer.document(app, record)),
            Some(keys) => self.peer.opened_document(app, record, &mut keys.open()),
        }
    }
}

/// Publish `changes` to `record` of `app` into `candidate` as `key`'s write:
/// sealed with `keys` for a sealed record, in the clear otherwise.
#[cfg(feature = "agents")]
pub(super) fn publish_changes(
    candidate: &mut StatePeer,
    keys: Option<&RecordKeys>,
    (app, record): (&str, &str),
    changes: Vec<u8>,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    match keys {
        None => candidate.publish_incremental(app, record, changes, key, now),
        Some(keys) => candidate.publish_sealed_incremental(
            app,
            record,
            changes,
            &mut keys.seal(ContentRetention::Delta),
            key,
            now,
        ),
    }
}

/// Publish `changes` a bot's device `bot` made to a sealed record into
/// `candidate`: decided as the bot's own sealed write, sealed with `keys`
/// and published under the governor's `key`, which the room's members take
/// it from, as they take a bot's changes to a record in the clear from the
/// governor's checkpoint.
#[cfg(feature = "agents")]
pub(super) fn publish_changes_for(
    candidate: &mut StatePeer,
    keys: &RecordKeys,
    bot: &SigningKey,
    route: (&str, &str),
    changes: Vec<u8>,
    key: &SigningKey,
    now: SystemTime,
) -> Result<NeighbourMsg, String> {
    candidate.publish_sealed_incremental_for(
        bot,
        route,
        changes,
        &mut keys.seal(ContentRetention::Delta),
        key,
        now,
    )
}

impl Rooms {
    /// Compact every sealed record history of `room` that has used most of
    /// its bound into one sealed checkpoint, and commit and publish the
    /// compactions. A record whose history does not merge yet (a write it
    /// depends on has not arrived) waits for a later call.
    ///
    /// # Errors
    ///
    /// The commit or its publication fails.
    pub(super) async fn compact_sealed_histories(
        &self,
        room: &mut GovernedRoom,
    ) -> Result<(), String> {
        let due = room.peer().sealed_records_to_compact();
        if due.is_empty() {
            return Ok(());
        }
        if room.compaction_wait > 0 {
            room.compaction_wait = room.compaction_wait.saturating_sub(1);
            return Ok(());
        }
        let now = SystemTime::now();
        let mut candidate = room.peer().clone();
        let mut compacted = 0usize;
        for (app, record) in &due {
            let compaction = match room.record_keys(app, record).await {
                Ok(Some(keys)) => candidate.compact_sealed_history(
                    app,
                    record,
                    &mut keys.open(),
                    &mut keys.seal(ContentRetention::Checkpoint),
                    &self.inner.host.key,
                    now,
                ),
                Ok(None) => continue,
                Err(error) => Err(error),
            };
            match compaction {
                Ok(_) => compacted = compacted.saturating_add(1),
                Err(error) => {
                    room.compaction_wait = COMPACTION_RETRY;
                    tracing::warn!(room = %room.room(), %app, %record, %error,
                        "a sealed record history was not compacted");
                }
            }
        }
        if compacted == 0 {
            return Ok(());
        }
        let collaboration = room.collaboration().clone();
        room.commit(candidate, &self.inner.host.key, now, |_, _| {
            Ok(collaboration)
        })?;
        self.settle(room).await
    }
}

#[cfg(all(test, feature = "agents"))]
pub(crate) mod tests;
