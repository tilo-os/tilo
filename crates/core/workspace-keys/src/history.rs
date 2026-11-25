//! Maintaining a sealed record's history: opening any of its writes, and
//! sealing the checkpoint that replaces the ones it covers.
use collab_types::{EncryptionMetadata, SignedOp};
use key_group::ContentKeys;
use tilo_desktop_model::WorkspaceId;

use crate::encryption::{KeyAccess, SharedKeys};

/// The keys that open a sealed record's history as a reader took them from
/// the service once ([`SharedKeys::history_keys`]): every group it held
/// then, and no lock of the service's.
///
/// A reader that opens a record's history while it holds a lock of the
/// workspace's own takes these first, so the key service is never asked
/// for its keys under that lock: the service answers those questions by
/// reading the workspace's governance, and its pump settles the groups under
/// the service's lock and reads that governance there, so a reader waiting on
/// the service while the pump waits on the workspace would wait forever.
#[derive(Clone)]
pub struct HistoryKeys(ContentKeys);

impl From<ContentKeys> for HistoryKeys {
    /// Keys a reader was handed whole rather than took from its own service: the record's host
    /// hands a device that takes no part in the record's group the keys of a record it acts on.
    fn from(keys: ContentKeys) -> Self {
        Self(keys)
    }
}

impl HistoryKeys {
    /// Which keys these are: the workspace they open and the epoch they seal under now. A rotation,
    /// which removing a member makes, moves the epoch, so what a reader opened under earlier keys
    /// is told apart from what it opens under these.
    #[must_use]
    pub fn generation(&self) -> (WorkspaceId, u64) {
        (self.0.workspace(), self.0.epoch())
    }

    /// Open `envelope`, a write of the record's history (a compaction
    /// included), under any epoch these keys carry.
    pub fn open(&self, envelope: &SignedOp) -> Result<Vec<u8>, String> {
        let metadata = envelope.encryption.ok_or("the write is not sealed")?;
        let ciphertext =
            collab_types::sealed_ciphertext(envelope).map_err(|error| error.to_string())?;
        self.0
            .open_content(envelope.artifact, &metadata, &ciphertext)
            .map(|plaintext| plaintext.to_vec())
            .map_err(|error| error.to_string())
    }

    /// Seal `cleartext`, the merged history of `app`'s `document`, as that document's checkpoint
    /// under the epoch these keys seal with now. The plaintext never leaves this call.
    pub fn seal_checkpoint(
        &self,
        workspace: WorkspaceId,
        (app, document): (&str, &str),
        cleartext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let artifact = collab_types::document_artifact_id(app, &workspace.to_string(), document);
        // A compaction's snapshot too large for one op goes as segments, the
        // later ones following its first in a store's retained tail.
        self.0
            .seal_content(
                artifact,
                collab_types::ContentRetention::of_frame(cleartext),
                cleartext,
            )
            .map_err(|error| error.to_string())
    }

    /// The payload a renewal of `envelope`, a sealed write of this device, carries under a
    /// fresh timestamp and sequence: the identical ciphertext while its epoch is the one these
    /// keys seal with, else its content re-sealed under that epoch. The plaintext never leaves
    /// this call.
    pub fn renew(&self, envelope: &SignedOp) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let metadata = envelope.encryption.ok_or("the write is not sealed")?;
        let retention = metadata
            .retention()
            .ok_or("the write is not sealed with a content key")?;
        if metadata.epoch == self.0.epoch() {
            return Ok((metadata, envelope.payload.clone()));
        }
        let plaintext = self.open(envelope)?;
        let (metadata, ciphertext) = self
            .0
            .seal_content(envelope.artifact, retention, &plaintext)
            .map_err(|error| error.to_string())?;
        // A checkpoint renewed still names the writes it holds.
        if collab_types::is_sealed_checkpoint(envelope) {
            let floors = collab_types::SealedCheckpoint::parse(envelope)
                .map_err(|error| error.to_string())?
                .floors;
            let payload = collab_types::SealedCheckpoint::new(floors, ciphertext)
                .encode()
                .map_err(|error| error.to_string())?;
            return Ok((metadata, payload));
        }
        Ok((metadata, ciphertext))
    }
}

impl SharedKeys {
    /// `workspace`'s keys as this device holds them now, for opening a
    /// sealed record's history outside the service's lock ([`HistoryKeys`]).
    /// `None` when this device takes part in no group of the workspace;
    /// [`Err`] when it does and holds none yet.
    #[must_use]
    pub fn history_keys(&self, workspace: WorkspaceId) -> Option<Result<HistoryKeys, String>> {
        self.with_service(|service| service.history_keys(workspace))
            .unwrap_or_else(|| Some(Err("the key service is unavailable".into())))
            .map(|keys| keys.map(HistoryKeys))
    }

    /// Open `envelope`, a write of a sealed record's history (a compaction
    /// included), under any epoch whose key this device holds: what a key
    /// holder compacting the history reads. Call it only for writes already
    /// admitted with every envelope check.
    pub fn open_history_write(
        &self,
        workspace: WorkspaceId,
        envelope: &SignedOp,
    ) -> Result<Vec<u8>, String> {
        let group = envelope
            .encryption
            .ok_or("the write is not sealed")?
            .key_group(workspace);
        HistoryKeys(self.held_history_keys(group, None)?).open(envelope)
    }

    /// Seal a checkpoint in the document's group after comparing its current
    /// key holders with the reader roster from the owner's policy snapshot.
    pub fn seal_scoped_checkpoint(
        &self,
        workspace: WorkspaceId,
        document: (&str, &str),
        named: Option<&str>,
        readers: &std::collections::BTreeSet<[u8; 32]>,
        cleartext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        self.scoped_history_keys(workspace, document, named, readers)?
            .seal_checkpoint(workspace, document, cleartext)
    }

    /// Renew a held write under the document's current group, including a
    /// write held since before the document received its own group.
    pub fn renew_scoped_document(
        &self,
        workspace: WorkspaceId,
        document: (&str, &str),
        named: Option<&str>,
        readers: &std::collections::BTreeSet<[u8; 32]>,
        write: &SignedOp,
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let metadata = write.encryption.ok_or("the write is not sealed")?;
        let target = self.scoped_history_keys(workspace, document, named, readers)?;
        let previous =
            collab_types::reader_group(workspace, named.unwrap_or(collab_types::EVERY_MEMBER))
                .ok_or("invalid reader set for write renewal")?;
        if metadata.key_group(previous) == target.0.workspace() {
            return target.renew(write);
        }
        let cleartext = self.open_history_write(previous, write)?;
        let retention = metadata
            .retention()
            .ok_or("the write has no content retention")?;
        target
            .0
            .seal_content(write.artifact, retention, &cleartext)
            .map_err(|error| error.to_string())
    }

    fn scoped_history_keys(
        &self,
        workspace: WorkspaceId,
        (app, document): (&str, &str),
        named: Option<&str>,
        readers: &std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<HistoryKeys, String> {
        let group = neighbour_wire::KeyScope::document(workspace, app, document, named)
            .ok_or("readers do not match the document")?
            .group();
        self.held_history_keys(group, Some(readers))
            .map(HistoryKeys)
    }

    /// Seal `cleartext`, the merged history of a sealed record, as that
    /// record's checkpoint under its group's current epoch: what a device
    /// compacting a history it takes part in publishes in its place. The
    /// plaintext never leaves this call.
    pub fn seal_checkpoint(
        &self,
        workspace: WorkspaceId,
        app: &str,
        document: &str,
        named: Option<&str>,
        cleartext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let group =
            collab_types::reader_group(workspace, named.unwrap_or(collab_types::EVERY_MEMBER))
                .ok_or("invalid reader set for compaction")?;
        let keys = if named.is_some() {
            self.held_history_keys(group, None)?
        } else {
            let (access, _) = self.access(group);
            let KeyAccess::Ready(keys) = access else {
                return Err("this device holds no current key for the document's group".into());
            };
            keys
        };
        HistoryKeys(keys).seal_checkpoint(workspace, (app, document), cleartext)
    }
}
