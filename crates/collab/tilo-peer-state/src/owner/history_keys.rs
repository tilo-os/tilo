//! Keys used to compact the sealed history of workspace documents.

use super::{StatePeer, WorkspaceId};
use collab_types::{EncryptionMetadata, SignedOp};
use web_time::SystemTime;

/// The channel keys a device holds for the sealed records of its workspaces:
/// what it compacts their histories with
/// ([`NativeWorkspaceOwner::keep_sealed_histories`](super::NativeWorkspaceOwner::keep_sealed_histories)).
pub trait SealedHistoryKeys: collab_types::signer::MaybeSendSync {
    /// Open `write`, an admitted write of a sealed record's history of
    /// `workspace` (a compaction included), under any epoch whose key this
    /// device holds.
    ///
    /// # Errors
    /// This device holds no key that opens it.
    fn open(&self, workspace: WorkspaceId, write: &SignedOp) -> Result<Vec<u8>, String>;
    /// The declared readers retained for a named store document.
    fn named_readers(&self, _workspace: WorkspaceId, _document: &str) -> Option<String> {
        None
    }
    /// Seal `snapshot`, the merged history of `app`'s `record`, as that
    /// record's checkpoint under its channel's current epoch.
    ///
    /// # Errors
    /// This device holds no current key of the record's channel.
    fn seal_checkpoint(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
        named: Option<&str>,
        snapshot: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String>;
    /// Seal under the document's group when its reader roster is narrower.
    fn seal_scoped_checkpoint(
        &self,
        _workspace: WorkspaceId,
        _document: (&str, &str),
        _named: Option<&str>,
        _readers: &std::collections::BTreeSet<[u8; 32]>,
        _snapshot: &[u8],
    ) -> Result<(EncryptionMetadata, Vec<u8>), String> {
        Err("this device cannot seal a checkpoint for a restricted document yet".into())
    }
}

pub(super) fn seal_for_document(
    keys: &dyn SealedHistoryKeys,
    peer: &StatePeer,
    workspace: WorkspaceId,
    document: (&str, &str),
    named: Option<&str>,
    snapshot: &[u8],
    now: SystemTime,
) -> Result<(EncryptionMetadata, Vec<u8>), String> {
    if peer.requires_document_keys(document.0, document.1, now)? {
        let readers = peer.document_key_holders_read_by(document, named, now)?;
        keys.seal_scoped_checkpoint(workspace, document, named, &readers, snapshot)
    } else {
        keys.seal_checkpoint(workspace, document.0, document.1, named, snapshot)
    }
}
