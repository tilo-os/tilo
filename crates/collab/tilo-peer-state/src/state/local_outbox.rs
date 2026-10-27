//! Local immutable publication custody and current-authorized history recovery.
//! Nothing here admits remote historical wire data or commits an owner's storage.
use super::{
    AdmissionError, ArtifactKind, BTreeMap, ChannelState, Document, DocumentAdmission,
    DocumentMutation, Duration, Ed25519PublicKey, MemberAuthority, NeighbourMsg, OpKind, ReadDoc,
    Route, SignedOp, SigningKey, StatePeer, SystemTime, Transactable, err, peer_handle, room,
};
use crate::state::facades::{PeerOutbox, PeerOutboxMut};

/// A channel key holder's renewal of a sealed write: the encryption and
/// payload a renewal carries under the channel's current epoch, or a refusal.
pub type Reseal<'a> =
    dyn FnMut(&SignedOp) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> + 'a;

const MAX_PUBLICATIONS: usize = 64;
const MAX_PUBLICATION_BYTES: usize = 4 * 1024 * 1024;

struct LocalDocument {
    admission: DocumentAdmission,
    route: Route,
    /// `None` for a sealed write: this replica holds only its ciphertext, so
    /// it has no history to check or project, only the channel's key holder
    /// can renew it, and it is retired by the governor's signed sequence
    /// watermark alone.
    mutation: Option<DocumentMutation>,
}

impl StatePeer {
    fn local_document_parts(
        &self,
        message: &NeighbourMsg,
        selected: [u8; 32],
    ) -> Result<LocalDocument, AdmissionError> {
        if !self.core.admission_v5 {
            return Err(AdmissionError::WriterSelectionRequired);
        }
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            record_id,
            envelope,
        } = message
        else {
            return Err("retained local publication requires AppDocument".into());
        };
        if cluster_id != &self.core.room
            || envelope.author.as_bytes() != &selected
            || *peer != peer_handle(&selected)
        {
            return Err("retained local publication selected author or room mismatch".into());
        }
        collab_types::verify_op(envelope).map_err(err)?;
        let route = Route {
            app: app_id.clone(),
            record: record_id.clone(),
        };
        let Some(document) = self.core.documents.get(&route) else {
            return Err("retained local publication route is not admitted".into());
        };
        let mutation = match collab_types::validate_whole_document_payload_route(
            app_id, cluster_id, record_id, envelope,
        )
        .map_err(err)?
        {
            collab_types::WholeDocumentPayload::Cleartext(mutation) => Some(mutation),
            collab_types::WholeDocumentPayload::Encrypted { .. }
                if document.encryption_required =>
            {
                None
            }
            collab_types::WholeDocumentPayload::Encrypted { .. } => {
                return Err(
                    "retained sealed publication for a record that is not encrypted".into(),
                );
            }
        };
        Ok(LocalDocument {
            admission: DocumentAdmission::from_op(cluster_id, app_id, record_id, envelope)?,
            route,
            mutation,
        })
    }

    fn retained_document(
        &self,
        message: &NeighbourMsg,
        selected: [u8; 32],
    ) -> Result<LocalDocument, String> {
        let local = self.local_document_parts(message, selected).map_err(err)?;
        let index = &self.core.exact_admissions;
        if index.get(&local.admission.key) != Some(&local.admission.operation_id)
            && !index.covers(&local.admission.key)
        {
            return Err("original publication has no exact retained admission".into());
        }
        Ok(local)
    }

    // A partial incremental's dependencies come only from this actually admitted
    // record. Comparing resulting change hashes distinguishes equal text from
    // genuine causal inclusion, and permits equivalent document/change encodings.
    /// `local` with every change it depends on from the admitted record:
    /// what a recovery re-sends for it (see [`history`]).
    fn publication_history(
        &self,
        local: &LocalDocument,
    ) -> Result<std::sync::Arc<automerge::Automerge>, String> {
        let current = self
            .core
            .documents
            .get(&local.route)
            .ok_or("retained publication record missing")?;
        let mutation = local
            .mutation
            .as_ref()
            .ok_or("a sealed publication has no cleartext history")?;
        history::of(
            history::key(mutation),
            mutation,
            current.doc.committed_document(),
        )
    }

    fn fresh_local_recovery_checkpoint(
        &self,
        route: &Route,
        now: SystemTime,
    ) -> Result<bool, String> {
        if !self.fresh_checkpoint_for(&route.app, &route.record, now) {
            return Ok(false);
        }
        let document = self
            .core
            .documents
            .get(route)
            .ok_or("recovery record missing")?;
        if document.encryption_required {
            return Err("recovery needs actual cleartext record custody".into());
        }
        let Some(snapshot) = &document.covered_snapshot else {
            return Ok(false);
        };
        let coverage = crate::coverage::entries(
            self.core
                .room_doc
                .as_deref()
                .ok_or("missing room governance")?,
        )?;
        let Some(coverage) = coverage.get(&crate::coverage::route_key(&route.app, &route.record))
        else {
            return Ok(false);
        };
        let accepted = snapshot.decoded()?;
        Ok(coverage.room == self.core.room
            && coverage.app == route.app
            && coverage.record == route.record
            && coverage.author == self.core.authority.device_public_key
            && !self.core.revocations.is_revoked(&coverage.author)
            && accepted.heads == coverage.heads
            && accepted.payload_hash == coverage.payload_hash
            && coverage.watermarks.iter().all(|(author, sequence)| {
                document.covered.get(author).copied().unwrap_or(0) >= *sequence
            }))
    }

    // A successor may rely only on changes the governor has acknowledged, not
    // missing ancestors held solely in our local document. Otherwise a fresh
    // dependent frame would indefinitely suppress recovery of its predecessor.
    fn recovery_publication_history(
        &self,
        local: &LocalDocument,
    ) -> Result<Option<automerge::Automerge>, String> {
        let Some(DocumentMutation::Incremental(bytes)) = &local.mutation else {
            return Ok(None);
        };
        let mut history = automerge::Automerge::new();
        history.load_incremental(bytes).map_err(err)?;
        let checkpoint = self
            .core
            .documents
            .get(&local.route)
            .ok_or("recovery route is not admitted")?
            .covered_snapshot
            .as_ref()
            .ok_or("recovery checkpoint missing")?
            .decoded()?;
        loop {
            let missing = history.get_missing_deps(&[]);
            if missing.is_empty() {
                return Ok(Some(history));
            }
            let Some(dependencies) = missing
                .iter()
                .map(|hash| checkpoint.doc.get_change_by_hash(hash))
                .collect::<Option<Vec<_>>>()
            else {
                return Ok(None);
            };
            history.apply_changes(dependencies).map_err(err)?;
            if history.get_missing_deps(&[]) == missing {
                return Err("recovery publication dependencies did not advance".into());
            }
        }
    }

    fn validate_retained_history(&self, history: &StatePeer) -> Result<(), String> {
        if !self.core.admission_v5 || !history.core.admission_v5 {
            return Err("retained history requires selected native v5".into());
        }
        self.admissions_since(history).map_err(err)?;
        for (route, old) in &history.core.documents {
            let current = self
                .core
                .documents
                .get(route)
                .ok_or("retained history record missing")?;
            if old.encryption_required
                || current.encryption_required
                || !old
                    .doc
                    .base_heads()
                    .iter()
                    .all(|head| current.doc.get_change_by_hash(head).is_some())
            {
                return Err("retained history route or causal custody mismatch".into());
            }
        }
        Ok(())
    }

    fn retained_history_route_retired(
        &self,
        route: &Route,
        old: &Document,
    ) -> Result<bool, String> {
        let Some((receipt, accepted)) =
            self.durable_receipt_checkpoint(&route.app, &route.record)?
        else {
            return Ok(false);
        };
        Ok(old
            .doc
            .base_heads()
            .iter()
            .all(|head| accepted.doc.get_change_by_hash(head).is_some())
            && old.channel.last_seq_per_author.iter().all(|(author, seq)| {
                receipt
                    .coverage
                    .watermarks
                    .get(&hex::encode(author.as_bytes()))
                    .copied()
                    .unwrap_or(0)
                    >= *seq
            }))
    }
}
impl PeerOutbox<'_> {
    /// Historical exact custody only. This never grants current send/sign permission.
    pub fn lookup_retained_local_document(
        self,
        message: &NeighbourMsg,
        selected: [u8; 32],
    ) -> Result<Option<DocumentAdmission>, AdmissionError> {
        let local = self.peer.local_document_parts(message, selected)?;
        match self.peer.core.exact_admissions.get(&local.admission.key) {
            Some(id) if id == &local.admission.operation_id => Ok(Some(local.admission)),
            Some(_) => Err(AdmissionError::ConflictingSequence),
            // Folded into a compaction's snapshot: admitted.
            None if self.peer.core.exact_admissions.covers(&local.admission.key) => {
                Ok(Some(local.admission))
            }
            None => Ok(None),
        }
    }
    /// Only actual durable governor acknowledgement may retire retained bytes.
    pub fn retained_local_document_retired(
        self,
        message: &NeighbourMsg,
        selected: [u8; 32],
    ) -> Result<bool, String> {
        let local = self.peer.retained_document(message, selected)?;
        let covered = |receipt: &crate::durability::DurableReceipt| {
            receipt
                .coverage
                .watermarks
                .get(&hex::encode(selected))
                .copied()
                .unwrap_or(0)
                >= local.admission.sequence()
        };
        // A sealed record has no cleartext checkpoint to compare here.
        if local.mutation.is_none() {
            return Ok(self
                .peer
                .durable_receipt(&local.route.app, &local.route.record)?
                .is_some_and(|receipt| covered(&receipt)));
        }
        let Some((receipt, accepted)) = self
            .peer
            .durable_receipt_checkpoint(&local.route.app, &local.route.record)?
        else {
            return Ok(false);
        };
        if !covered(&receipt) {
            return Ok(false);
        }
        let history = self.peer.publication_history(&local)?;
        let accepted = accepted.doc.committed_document();
        Ok(history
            .get_heads()
            .iter()
            .all(|head| accepted.get_change_meta_by_hash(head).is_some()))
    }
    /// Compare retained real history with the admitted durable governor checkpoint.
    /// A test oracle for the recovery projection.
    #[cfg(test)]
    pub fn retained_local_history_retired(self, history: &StatePeer) -> Result<bool, String> {
        self.peer.validate_retained_history(history)?;
        for (route, old) in &history.core.documents {
            if !self.peer.retained_history_route_retired(route, old)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
impl PeerOutboxMut<'_> {
    /// Renew `original`, a sealed write of the selected device still in
    /// custody, once it expired before the governor's watermark retired it.
    /// `reseal` is the channel's key holder: it returns the payload to carry
    /// under the channel's current epoch, or refuses. The renewal keeps the
    /// original's artifact, attestation and class, is signed at `now` under
    /// the author's next sequence, and is admitted as a new local sealed
    /// write. `None` when `original` is not due; on an error nothing changes.
    pub fn renew_local_sealed(
        self,
        original: &NeighbourMsg,
        reseal: &mut Reseal<'_>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Option<NeighbourMsg>, String> {
        let selected = key.verifying_key().to_bytes();
        let local = self.peer.retained_document(original, selected)?;
        if local.mutation.is_some()
            || crate::outbox::fresh(original, now)
            || self
                .peer
                .outbox()
                .retained_local_document_retired(original, selected)?
        {
            return Ok(None);
        }
        self.peer.permissions(selected, now)?;
        let NeighbourMsg::AppDocument { envelope, .. } = original else {
            return Err("retained local publication requires AppDocument".into());
        };
        let (encryption, payload) = reseal(envelope)?;
        if Some(encryption.channel) != envelope.encryption.map(|sealed| sealed.channel) {
            return Err("a renewal must stay on its write's channel".into());
        }
        let seq = self
            .peer
            .core
            .documents
            .get(&local.route)
            .and_then(|document| document.channel.last_seq_per_author.get(&envelope.author))
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("sequence exhausted")?;
        let mut renewal = SignedOp::new_unsigned_with_encryption(
            payload,
            envelope.artifact,
            envelope.author,
            envelope.attestation_id,
            envelope.kind,
            now,
            seq,
            encryption,
        )
        .with_claims(envelope.claims.clone());
        collab_types::sign_op(&mut renewal, key).map_err(err)?;
        let mut candidate = self.peer.clone();
        candidate.admit_new_local_document(
            &local.route.app,
            &local.route.record,
            renewal.clone(),
            now,
        )?;
        let message = NeighbourMsg::AppDocument {
            peer: peer_handle(&selected),
            app_id: local.route.app,
            cluster_id: candidate.core.room.clone(),
            record_id: local.route.record,
            envelope: Box::new(renewal),
        };
        candidate.retained_document(&message, selected)?;
        *self.peer = candidate;
        Ok(Some(message))
    }
}

// Count the closed wire DTO before cloning the caller's prefix. These are encoded
// refusal limits, not a native CRDT heap qualification or a durable barrier.
fn publication_size(messages: &[NeighbourMsg]) -> Result<usize, String> {
    if messages.len() > MAX_PUBLICATIONS {
        return Err("local publication count limit".into());
    }
    messages.iter().try_fold(0usize, |total, message| {
        let room = MAX_PUBLICATION_BYTES.saturating_sub(total);
        crate::postcard_sink::encoded_len(message, room)
            .map(|size| total.saturating_add(size))
            .map_err(|_| "local publication byte limit".into())
    })
}

#[cfg(test)]
mod sealed_renewal_tests;
#[cfg(test)]
mod tests;

mod history;
mod projection;
pub use projection::LocalRecoveryReservation;
