//! The governor's own reads and writes of records its members write sealed.
//!
//! In a room whose channels this relay stewards, members seal every write
//! of a record, and the governor's copy keeps those writes as the record's
//! ciphertext history ([`StatePeer::is_sealed_record`]). As the channels'
//! steward the relay holds their content keys: it reads such a record by
//! opening that history over the record's cleartext base, and writes to it
//! sealed like any member does.

use channel_crypto::ChannelContentKeys;
use collab_types::{ArtifactId, ContentRetention, EncryptionMetadata, SignedOp};
use ed25519_dalek::SigningKey;
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::StatePeer;

use super::{GovernedRoom, SystemTime};

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

#[cfg(all(test, feature = "agents"))]
mod tests;
