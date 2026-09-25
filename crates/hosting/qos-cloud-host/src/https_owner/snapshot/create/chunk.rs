use super::*;
use qos_relay::delivery_v3::{
    framing::{MessageType, append_frame},
    state::{CommitBoundary, SNAPSHOT_CHUNK_BYTES, SnapshotChunk},
};
const WORK_BYTES: usize = 1024 * 1024;
const OUTPUT_BYTES: usize = 272 * 1024;
pub(in crate::https_owner) struct PreparedChunk {
    live: LiveSnapshot,
    bytes: Vec<u8>,
    index: u32,
    requested_digest: Digest,
    credit: ByteCredit,
}
pub(in crate::https_owner) struct ChunkReply {
    bytes: Vec<u8>,
    _credit: ByteCredit,
}
impl ChunkReply {
    pub(in crate::https_owner) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
/// Refusal retains only the caller's original actor; it creates no authority.
pub(in crate::https_owner) struct ChunkPrepareRefusal {
    live: LiveSnapshot,
    error: CloudDeliveryError,
}
impl std::fmt::Debug for ChunkPrepareRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ChunkPrepareRefusal")
            .field(&self.error)
            .finish()
    }
}
impl ChunkPrepareRefusal {
    pub(in crate::https_owner) fn into_parts(self) -> (LiveSnapshot, CloudDeliveryError) {
        (self.live, self.error)
    }
}
impl LiveSnapshot {
    /// Worker phase: no live Store turn or registry borrow crosses this work.
    pub(in crate::https_owner) fn prepare_chunk(
        self,
        index: u32,
        requested_digest: Digest,
    ) -> std::result::Result<PreparedChunk, ChunkPrepareRefusal> {
        let prepared = (|| -> Result<(Vec<u8>, ByteCredit)> {
            let pair = SnapshotManifestPair::decode(&self.draft.pair.bytes)
                .map_err(|_| CloudDeliveryError::InvalidInput)?;
            // Previous manifests are retry anchors for Renew only. Reject a
            // changed chunk target before acquiring any worker/output credit.
            require(
                *blake3::hash(pair.current()).as_bytes() == requested_digest,
                CloudDeliveryError::Denied,
            )?;
            let credit = self
                .draft
                .prepared
                .selection
                .pool
                .try_acquire_bulk(WORK_BYTES)
                .ok_or(CloudDeliveryError::Capacity)?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(SNAPSHOT_CHUNK_BYTES)
                .map_err(|_| CloudDeliveryError::Capacity)?;
            require(
                bytes.capacity() <= SNAPSHOT_CHUNK_BYTES,
                CloudDeliveryError::Capacity,
            )?;
            bytes.resize(SNAPSHOT_CHUNK_BYTES, 0);
            let count = self
                .draft
                .prepared
                .stream
                .fill_chunk(index, &mut bytes)
                .map_err(|_| CloudDeliveryError::InvalidInput)?;
            bytes.truncate(count);
            Ok((bytes, credit))
        })();
        match prepared {
            Ok((bytes, credit)) => Ok(PreparedChunk {
                live: self,
                bytes,
                index,
                requested_digest,
                credit,
            }),
            Err(error) => Err(ChunkPrepareRefusal { live: self, error }),
        }
    }
}
impl PreparedChunk {
    pub(in crate::https_owner) fn authorize(
        mut self,
        turn: &CloudStoreTurn,
        session: &VerifiedSession,
    ) -> Result<(LiveSnapshot, ChunkReply)> {
        let manifest = self.live.manifest(turn, session)?;
        let digest = *blake3::hash(manifest).as_bytes();
        require(digest == self.requested_digest, CloudDeliveryError::Denied)?;
        let prepared = &self.live.draft.prepared;
        let scope = &prepared.selection.scope;
        let encoded = SnapshotChunk::new(prepared.selection.pin, digest, self.index, &self.bytes)
            .and_then(|chunk| chunk.encode())
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
        require(
            encoded.capacity() <= 256 * 1024,
            CloudDeliveryError::Capacity,
        )?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(OUTPUT_BYTES)
            .map_err(|_| CloudDeliveryError::Capacity)?;
        require(
            output.capacity() <= OUTPUT_BYTES,
            CloudDeliveryError::Capacity,
        )?;
        append_frame(
            &mut output,
            MessageType::SnapshotChunk,
            &encoded,
            OUTPUT_BYTES,
        )
        .map_err(|_| CloudDeliveryError::Capacity)?;
        if self.index as usize + 1 == prepared.stream.chunk_digests().len() {
            let cursor = turn.sign_cursor(scope.clone())?;
            let boundary = CommitBoundary::new(
                scope.consumer,
                scope.authorization_view,
                scope.store_generation,
                scope.committed.clone(),
                prepared
                    .stream
                    .item_count()
                    .try_into()
                    .map_err(|_| CloudDeliveryError::Capacity)?,
                prepared.stream.visible_order(),
                cursor,
            )
            .and_then(|boundary| boundary.encode())
            .map_err(|_| CloudDeliveryError::InvalidInput)?;
            require(
                boundary.capacity() <= 16 * 1024,
                CloudDeliveryError::Capacity,
            )?;
            append_frame(
                &mut output,
                MessageType::CommitBoundary,
                &boundary,
                OUTPUT_BYTES,
            )
            .map_err(|_| CloudDeliveryError::Capacity)?;
        }
        // A complete encoded and freshly authorized reply advances only volatile
        // server progress. Neither chunk nor final boundary acknowledges receipt.
        self.live.served_through = self.live.served_through.max(u64::from(self.index) + 1);
        Ok((
            self.live,
            ChunkReply {
                bytes: output,
                _credit: self.credit,
            },
        ))
    }
}
#[cfg(test)]
#[path = "chunk/tests.rs"]
pub(super) mod tests;
