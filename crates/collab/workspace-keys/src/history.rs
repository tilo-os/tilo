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
}

impl SharedKeys {
    /// `workspace`'s keys as this device holds them now, for opening a
    /// sealed record's history outside the service's lock ([`HistoryKeys`]).
    /// `None` when this device takes part in no group of the workspace;
    /// [`Err`] when it does and holds none yet.
    #[must_use]
    pub fn history_keys(&self, workspace: WorkspaceId) -> Option<Result<HistoryKeys, String>> {
        self.record_keys(workspace)
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
        let keys = self
            .run(|service| service.record_keys(workspace))
            .ok_or("key service unavailable")?
            .ok_or("this device holds no existing key for the history's group")??;
        HistoryKeys(keys).open(envelope)
    }

    /// Seal `cleartext`, the merged history of a sealed record, as that
    /// record's checkpoint under its group's current epoch: what a device
    /// compacting a history it takes part in publishes in its place. The
    /// plaintext never leaves this call.
    pub fn seal_checkpoint(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
        cleartext: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        let name = workspace.to_string();
        let artifact = collab_types::document_artifact_id(app, &name, record);
        let (access, _) = self.access(workspace);
        let KeyAccess::Ready(keys) = access else {
            return Err("this device holds no current key for the record's group".into());
        };
        // A compaction's snapshot too large for one op goes as segments, the
        // later ones following its first in a store's retained tail.
        keys.seal_content(
            artifact,
            collab_types::ContentRetention::of_frame(cleartext),
            cleartext,
        )
        .map_err(|error| error.to_string())
    }
}
