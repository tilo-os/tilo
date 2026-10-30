//! Maintaining a sealed record's history: opening any of its writes, and
//! sealing the checkpoint that replaces the ones it covers.
use super::{ChannelAccess, ChannelKeyAccess, EncryptionMetadata, RoomId, SignedOp};

impl ChannelKeyAccess {
    /// Open `envelope`, a write of a sealed record's history (a compaction
    /// included), under any epoch whose key this device holds: what a key
    /// holder compacting the history reads. Call it only for writes already
    /// admitted with every envelope check.
    ///
    /// # Errors
    ///
    /// Refuses unavailable existing channel keys or epochs and invalid ciphertext.
    /// This read never creates a channel, requests access or rejoins a group.
    pub fn open_history_write(&self, room: RoomId, envelope: &SignedOp) -> Result<Vec<u8>, String> {
        let metadata = envelope.encryption.ok_or("the write is not sealed")?;
        let ciphertext =
            collab_types::sealed_ciphertext(envelope).map_err(|error| error.to_string())?;
        let answers = self.ask_host([(room, metadata.channel, None, false)]);
        let keys = self
            .with_answers(answers, |service| {
                service.existing_keys(room, metadata.channel)
            })
            .ok_or("channel service unavailable")?
            .ok_or("this device holds no existing key for the history's channel")?;
        keys.open_content(envelope.artifact, &metadata, &ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }

    /// Seal `cleartext`, the merged history of a sealed record, as that
    /// record's checkpoint under its channel's current epoch: what a device
    /// compacting a history it takes part in publishes in its place. The
    /// plaintext never leaves this call.
    ///
    /// # Errors
    ///
    /// This device holds no current key for the record's channel.
    pub fn seal_checkpoint(
        &self,
        room: RoomId,
        app: &str,
        record: &str,
        cleartext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let name = room.to_string();
        let channel = collab_types::document_channel_id(app, &name, record);
        let artifact = collab_types::document_artifact_id(app, &name, record);
        let (access, _) = self.access(room, channel);
        let ChannelAccess::Ready(keys) = access else {
            return Err("this device holds no current key for the record's channel".into());
        };
        keys.seal_content(
            artifact,
            collab_types::ContentRetention::Checkpoint,
            cleartext,
        )
        .map_err(|error| error.to_string())
    }
}
