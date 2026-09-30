//! A key holder compacts an encrypted record's sealed history; a replica
//! without the channel key accepts a verified compaction in place of exactly
//! the writes it covers, and never opens it.
use super::{
    DocumentMutation, Ed25519PublicKey, NeighbourMsg, OpClass, Route, SignedOp, SigningKey,
    StatePeer, SystemTime, err, peer_handle,
};
use collab_types::{ContentRetention, EncryptionMetadata, SealedCompaction};

type Floors = std::collections::HashMap<Ed25519PublicKey, u64>;
/// A key holder sealing a compaction's snapshot under the channel's current
/// epoch, with checkpoint retention.
pub type SealCheckpoint<'a> =
    dyn FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> + 'a;

impl StatePeer {
    /// Compact `record`'s sealed history as the key holder of its channel
    /// and supersede it here, returning the compaction to publish.
    ///
    /// `open` opens any write of the history, under any epoch whose key is
    /// held, and a previous compaction through
    /// [`collab_types::sealed_ciphertext`]. Every write is opened, decoded
    /// and merged over the record's cleartext base into one document; one
    /// that does not open, decode or merge completely aborts the whole
    /// compaction. `seal` seals that document's snapshot under the channel's
    /// current epoch with [`ContentRetention::Checkpoint`] retention. The
    /// compaction is signed by `key` and admitted here through the same
    /// checks every keyless replica applies, so `key` must hold the room's
    /// owner role.
    ///
    /// # Errors
    ///
    /// Returns an error when the record holds no sealed history, a write
    /// does not open or merge, the sealed snapshot is not a checkpoint for
    /// the record's channel or does not open to the merged record, or the
    /// compaction is refused by admission; this peer is unchanged then.
    pub fn compact_sealed_history(
        &mut self,
        app: &str,
        record: &str,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
        seal: &mut SealCheckpoint<'_>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let document = self
            .core
            .documents
            .get(&route)
            .ok_or("sealed compaction of a record this state does not hold")?;
        if document.sealed.is_empty() {
            return Err("the record holds no sealed history to compact".into());
        }
        let mut waiting = document
            .sealed
            .iter()
            .map(|write| DocumentMutation::decode(&open(write)?).map_err(err))
            .collect::<Result<Vec<_>, _>>()?;
        let mut merged = document.doc.clone();
        // A replica without the key admits sealed writes in arrival order,
        // not causal order: merge until nothing more applies.
        while !waiting.is_empty() {
            let before = waiting.len();
            let mut blocked = Vec::new();
            for mutation in waiting {
                match crate::document::merge_committed_with_status(&mut merged, &mutation)
                    .map_err(|error| format!("{error:?}"))?
                {
                    crate::document::MergeOutcome::Changed(_) => {}
                    crate::document::MergeOutcome::MissingDependencies => blocked.push(mutation),
                }
            }
            if blocked.len() == before {
                return Err("sealed history does not merge into a complete record".into());
            }
            waiting = blocked;
        }
        let snapshot = DocumentMutation::Snapshot(merged.save())
            .encode()
            .map_err(err)?;
        let (metadata, ciphertext) = seal(&snapshot)?;
        if metadata.retention() != Some(ContentRetention::Checkpoint)
            || metadata.channel != collab_types::document_channel_id(app, &self.core.room, record)
        {
            return Err("a sealed compaction is a checkpoint for the record's channel".into());
        }
        let payload = SealedCompaction::new(&document.sealed, ciphertext)
            .map_err(err)?
            .encode()
            .map_err(err)?;
        let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        let seq = document
            .channel
            .last_seq_per_author
            .get(&author)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("sequence exhausted")?;
        let mut op = SignedOp::new_unsigned_with_encryption(
            payload,
            collab_types::document_artifact_id(app, &self.core.room, record),
            author,
            collab_types::document_attestation_id(app, &self.core.room, record),
            OpClass::Compaction,
            now,
            seq,
            metadata,
        );
        collab_types::sign_op(&mut op, key).map_err(err)?;
        // Keyless replicas cannot tell an unreadable compaction from a good
        // one, so only one that opens to the merged record leaves here.
        if open(&op)? != snapshot {
            return Err("the sealed compaction does not open to the merged record".into());
        }
        let mut candidate = self.clone();
        candidate.admit_new_local_document(app, record, op.clone(), now)?;
        *self = candidate;
        Ok(NeighbourMsg::AppDocument {
            peer: peer_handle(author.as_bytes()),
            app_id: app.into(),
            cluster_id: self.core.room.clone(),
            record_id: record.into(),
            envelope: Box::new(op),
        })
    }
}

impl StatePeer {
    /// Every encrypted record whose sealed history has used three quarters
    /// of its bound, as `(app, record)`: its key holder compacts it before
    /// new writes are refused.
    #[must_use]
    pub fn sealed_records_to_compact(&self) -> Vec<(String, String)> {
        self.core
            .documents
            .iter()
            .filter(|(_, document)| {
                let bytes: usize = document.sealed.iter().map(|op| op.payload.len()).sum();
                document.sealed.len() >= super::MAX_SEALED_OPS / 4 * 3
                    || bytes >= super::MAX_SEALED_BYTES / 4 * 3
            })
            .map(|(route, _)| (route.app.clone(), route.record.clone()))
            .collect()
    }

    /// This replica's own sealed writes (the compactions it minted) that
    /// `before` does not hold. A governed snapshot cannot carry sealed
    /// history, so they are published as themselves.
    pub(super) fn own_sealed_publications(&self, before: Option<&Self>) -> Vec<NeighbourMsg> {
        let author = Ed25519PublicKey::from_bytes(self.core.authority.device_public_key);
        let mut messages = Vec::new();
        for (route, document) in &self.core.documents {
            let published = before
                .and_then(|before| before.core.documents.get(route))
                .map_or(&[][..], |prior| prior.sealed.as_slice());
            for op in &document.sealed {
                if op.author == author && !published.contains(op) {
                    messages.push(NeighbourMsg::AppDocument {
                        peer: peer_handle(author.as_bytes()),
                        app_id: route.app.clone(),
                        cluster_id: self.core.room.clone(),
                        record_id: route.record.clone(),
                        envelope: Box::new(op.clone()),
                    });
                }
            }
        }
        messages
    }
}

/// The history after `compaction`, whose envelope passed every admission
/// check, supersedes `history`: the compaction, then every write it does not
/// cover, in order. `floors` are the record's sequence floors before it.
pub(super) fn supersede(
    history: &[SignedOp],
    floors: &Floors,
    compaction: &SignedOp,
) -> Result<Vec<SignedOp>, String> {
    let wrapper = SealedCompaction::parse(compaction).map_err(err)?;
    let (superseded, kept): (Vec<&SignedOp>, Vec<&SignedOp>) =
        history.iter().partition(|write| wrapper.covers(write));
    if superseded.is_empty() {
        return Err("sealed compaction supersedes nothing this replica holds".into());
    }
    if covers_beyond(&wrapper, floors) {
        return Err("sealed compaction covers writes this replica has not admitted".into());
    }
    if kept
        .iter()
        .any(|write| write.op_class == OpClass::Compaction)
    {
        return Err("sealed compaction leaves an earlier compaction in place".into());
    }
    if collab_types::hash_sealed_prefix(superseded.iter().copied()).map_err(err)?
        != wrapper.prefix_hash
    {
        return Err(
            "sealed compaction is partial: it does not cover exactly this replica's writes".into(),
        );
    }
    let epoch = compaction.encryption.map_or(0, |meta| meta.epoch);
    if superseded
        .iter()
        .filter_map(|write| write.encryption)
        .any(|meta| meta.epoch > epoch)
    {
        return Err("sealed compaction is sealed under a stale epoch".into());
    }
    Ok(std::iter::once(compaction).chain(kept).cloned().collect())
}

/// Re-validate a stored history's compaction: only ever its first entry,
/// covering nothing beyond the record's floors and none of the later writes.
pub(super) fn check_stored(history: &[SignedOp], floors: &Floors) -> Result<(), String> {
    for (index, op) in history.iter().enumerate() {
        if op.op_class != OpClass::Compaction {
            continue;
        }
        if index != 0 {
            return Err("a sealed compaction is only ever first in a record's history".into());
        }
        let wrapper = SealedCompaction::parse(op).map_err(err)?;
        if covers_beyond(&wrapper, floors) {
            return Err("stored sealed compaction is ahead of the record's watermark".into());
        }
        if history.iter().skip(1).any(|write| wrapper.covers(write)) {
            return Err("stored sealed compaction covers a later write".into());
        }
    }
    Ok(())
}

fn covers_beyond(wrapper: &SealedCompaction, floors: &Floors) -> bool {
    wrapper
        .covers
        .iter()
        .any(|(author, seq)| floors.get(author).is_none_or(|floor| floor < seq))
}
