//! A key holder compacts an encrypted record's sealed history; a replica
//! without the channel key accepts a verified compaction in place of exactly
//! the writes it covers, and never opens it.
use super::{
    DocumentMutation, Ed25519PublicKey, NeighbourMsg, OpKind, Route, SignedOp, SigningKey,
    StatePeer, SystemTime, err, peer_handle,
};
use collab_types::{ContentRetention, EncryptionMetadata, SealedCompaction};

type Floors = std::collections::HashMap<Ed25519PublicKey, u64>;
/// A key holder sealing a compaction's snapshot under the channel's current
/// epoch, with checkpoint retention.
pub type SealCheckpoint<'a> =
    dyn FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> + 'a;
/// A key holder sealing a write to a record under the channel's current
/// epoch, with delta retention.
pub type SealDelta<'a> = SealCheckpoint<'a>;

/// Writes a sealed history must hold before it is compacted to make room in
/// a full exact admission index: fewer would fold little for the work.
pub(super) const PRESSED_HISTORY: usize = 64;

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
        let merged = merge_sealed(document, open)?;
        let snapshot = DocumentMutation::Snapshot(merged).encode().map_err(err)?;
        let (metadata, ciphertext) = seal(&snapshot)?;
        if metadata.retention() != Some(ContentRetention::Checkpoint)
            || metadata.channel != collab_types::document_channel_id(app, &self.core.room, record)
        {
            return Err("a sealed compaction is a checkpoint for the record's channel".into());
        }
        let payload = SealedCompaction::new(&document.sealed.to_vec(), ciphertext)
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
            OpKind::Compaction,
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

/// `document`'s cleartext base with every write of its sealed history opened
/// and merged over it, saved. A replica without the key admits sealed writes
/// in arrival order, not causal order: they merge until nothing more applies.
fn merge_sealed(
    document: &super::Document,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    // One isolated merge of every write, which takes them in causal order
    // whatever order they were admitted in: a merge per write copied the
    // whole record into the isolate each time.
    let mut history = Vec::new();
    for write in document.sealed.iter() {
        match DocumentMutation::decode(&open(write)?).map_err(err)? {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => {
                history.extend_from_slice(&bytes);
            }
        }
    }
    let mut merged = document.doc.clone();
    if history.is_empty() {
        return Ok(merged.save());
    }
    match crate::document::merge_committed_with_status(
        &mut merged,
        &DocumentMutation::Incremental(history),
    )
    .map_err(|error| format!("{error:?}"))?
    {
        crate::document::MergeOutcome::Changed(_) => Ok(merged.save()),
        crate::document::MergeOutcome::MissingDependencies => {
            Err("sealed history does not merge into a complete record".into())
        }
    }
}

impl StatePeer {
    /// `record` of `app` as its channel's key holder reads it: the cleartext
    /// base with every sealed write `open` opens merged over it. A record
    /// that is not encrypted reads as [`Self::document`]. `None` for a
    /// record this state does not hold.
    ///
    /// # Errors
    ///
    /// Returns an error when a sealed write does not open, decode or merge.
    pub fn opened_document(
        &self,
        app: &str,
        record: &str,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    ) -> Result<Option<Vec<u8>>, String> {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let Some(document) = self.core.documents.get(&route) else {
            return Ok(None);
        };
        if !document.encryption_required {
            return Ok(self.document(app, record));
        }
        merge_sealed(document, open).map(Some)
    }

    /// Whether `record` of `app` is encrypted here: its writes are sealed,
    /// and a write in the clear is refused as a downgrade.
    #[must_use]
    pub fn is_sealed_record(&self, app: &str, record: &str) -> bool {
        self.sealed_route(&Route {
            app: app.into(),
            record: record.into(),
        })
    }

    /// Sign `changes`, Automerge changes to the encrypted `record` of `app`,
    /// as `key`'s own write, sealed by the channel's key holder with `seal`,
    /// and admit it like any sealed write: it joins the record's sealed
    /// history, and the next durable publication carries it as itself.
    /// Members open it as they open each other's writes.
    ///
    /// # Errors
    ///
    /// Returns an error for a record that is not encrypted (it is written in
    /// the clear), when sealing fails or yields anything but a delta of the
    /// record's channel, or when admission refuses the write; this peer is
    /// unchanged then.
    pub fn publish_sealed_incremental(
        &mut self,
        app: &str,
        record: &str,
        changes: Vec<u8>,
        seal: &mut SealDelta<'_>,
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
            .ok_or("sealed write to a record this state does not hold")?;
        if !document.encryption_required {
            return Err("the record is not encrypted: write it in the clear".into());
        }
        let mutation = DocumentMutation::Incremental(changes);
        // Readers without the key decide a sealed write by the claims of
        // its changes, as a member's page names them.
        let claims =
            collab_types::mutation_claims(&mutation, collab_types::record_unit).map_err(err)?;
        let plaintext = mutation.encode().map_err(err)?;
        let (metadata, ciphertext) = seal(&plaintext)?;
        if metadata.retention() != Some(ContentRetention::Delta)
            || metadata.channel != collab_types::document_channel_id(app, &self.core.room, record)
        {
            return Err("a sealed write is a delta of the record's channel".into());
        }
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
            ciphertext,
            collab_types::document_artifact_id(app, &self.core.room, record),
            author,
            collab_types::document_attestation_id(app, &self.core.room, record),
            OpKind::Change,
            now,
            seq,
            metadata,
        )
        .with_claims(claims)
        .with_surface(self.app_surfaces.0.as_ref().and_then(|of| of(app)));
        collab_types::sign_op(&mut op, key).map_err(err)?;
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

    /// [`Self::publish_sealed_incremental`] for `changes` a device this
    /// state's authority signs for made, such as a bot it installed
    /// (`delegate`): the room decides them as `delegate`'s own sealed write
    /// (its grants over the claims they name, and the name they show), and
    /// the authority's `key` publishes them, as the authority's checkpoint
    /// carries such a device's changes to a record written in the clear.
    ///
    /// # Errors
    ///
    /// As [`Self::publish_sealed_incremental`], and when the room would
    /// refuse `delegate`'s own write of `changes`; this peer is unchanged
    /// then.
    pub fn publish_sealed_incremental_for(
        &mut self,
        delegate: &SigningKey,
        (app, record): (&str, &str),
        changes: Vec<u8>,
        seal: &mut SealDelta<'_>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let author = delegate.verifying_key().to_bytes();
        let parsed = collab_types::parse_changes(&changes).map_err(err)?;
        let shows = self.appearance_author(&author, now);
        collab_types::check_appearances(&parsed, shows.as_deref())
            .map_err(|refusal| format!("document change forbidden: {refusal}"))?;
        self.clone().publish_sealed_incremental(
            app,
            record,
            changes.clone(),
            seal,
            delegate,
            now,
        )?;
        self.publish_sealed_incremental(app, record, changes, seal, key, now)
    }

    /// Every encrypted record whose sealed history has used three quarters
    /// of its bound, as `(app, record)`: its key holder compacts it before
    /// new writes are refused.
    #[must_use]
    pub fn sealed_records_to_compact(&self) -> Vec<(String, String)> {
        let mut due: Vec<(String, String)> = self
            .core
            .documents
            .iter()
            .filter(|(_, document)| {
                document.sealed.len() >= super::MAX_SEALED_OPS / 4 * 3
                    || document.sealed.bytes() >= super::MAX_SEALED_BYTES / 4 * 3
            })
            .map(|(route, _)| (route.app.clone(), route.record.clone()))
            .collect();
        // Every sealed write leaves exact admission evidence until a
        // compaction folds it into its writer's mark. Many records each
        // short of their own bound (the parts of a chained record) fill the
        // room's index all the same: once it is half full, the longest
        // history worth folding is due too, one at a time, so the work each
        // write brings stays that of one record.
        if due.is_empty() && self.core.exact_admissions.pressed() {
            due.extend(
                self.core
                    .documents
                    .iter()
                    .filter(|(_, document)| document.sealed.len() >= PRESSED_HISTORY)
                    .max_by_key(|(_, document)| document.sealed.len())
                    .map(|(route, _)| (route.app.clone(), route.record.clone())),
            );
        }
        due
    }

    /// This replica's own sealed writes (the compactions it minted) that
    /// `before` does not hold. A governed snapshot cannot carry sealed
    /// history, so they are published as themselves.
    pub(super) fn own_sealed_publications(&self, before: Option<&Self>) -> Vec<NeighbourMsg> {
        let author = Ed25519PublicKey::from_bytes(self.core.authority.device_public_key);
        let mut messages = Vec::new();
        for (route, document) in &self.core.documents {
            let prior = before.and_then(|before| before.core.documents.get(route));
            // A record still shared with `before` holds nothing new.
            if prior.is_some_and(|prior| super::backend::StoredDocument::ptr_eq(prior, document)) {
                continue;
            }
            let published: Vec<&SignedOp> = prior
                .map(|prior| {
                    prior
                        .sealed
                        .iter()
                        .filter(|op| op.author == author)
                        .collect()
                })
                .unwrap_or_default();
            for op in document.sealed.iter().filter(|op| op.author == author) {
                if !published.contains(&op) {
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

/// Each author's highest write `compaction` folds into its snapshot.
pub(super) fn covered(compaction: &SignedOp) -> Result<Vec<([u8; 32], u64)>, String> {
    let wrapper = SealedCompaction::parse(compaction).map_err(err)?;
    Ok(wrapper
        .covers
        .iter()
        .map(|(author, sequence)| (*author.as_bytes(), *sequence))
        .collect())
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
    if kept.iter().any(|write| write.kind == OpKind::Compaction) {
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
        if op.kind != OpKind::Compaction {
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
