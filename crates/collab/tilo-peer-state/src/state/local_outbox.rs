//! Local immutable publication custody and current-authorized history recovery.
//! Nothing here admits remote historical wire data or commits an owner's storage.
use super::{
    AdmissionError, ArtifactKind, BTreeMap, ChannelState, Document, DocumentAdmission,
    DocumentMutation, Duration, Ed25519PublicKey, MemberAuthority, NeighbourMsg, OpClass, ReadDoc,
    Route, SignedOp, SigningKey, StatePeer, SystemTime, Transactable, err, peer_handle, room,
};

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

    /// Historical exact custody only. This never grants current send/sign permission.
    ///
    /// # Errors
    ///
    /// Returns an [`AdmissionError`] if the message is not a valid retained document of the
    /// selected device.
    pub fn lookup_retained_local_document(
        &self,
        message: &NeighbourMsg,
        selected: [u8; 32],
    ) -> Result<Option<DocumentAdmission>, AdmissionError> {
        let local = self.local_document_parts(message, selected)?;
        match self.core.exact_admissions.get(&local.admission.key) {
            Some(id) if id == &local.admission.operation_id => Ok(Some(local.admission)),
            Some(_) => Err(AdmissionError::ConflictingSequence),
            None => Ok(None),
        }
    }

    fn retained_document(
        &self,
        message: &NeighbourMsg,
        selected: [u8; 32],
    ) -> Result<LocalDocument, String> {
        let local = self.local_document_parts(message, selected).map_err(err)?;
        if self.core.exact_admissions.get(&local.admission.key)
            != Some(&local.admission.operation_id)
        {
            return Err("original publication has no exact retained admission".into());
        }
        Ok(local)
    }

    /// Only actual durable governor acknowledgement may retire retained bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the message is not a retained document of the selected device.
    pub fn retained_local_document_retired(
        &self,
        message: &NeighbourMsg,
        selected: [u8; 32],
    ) -> Result<bool, String> {
        let local = self.retained_document(message, selected)?;
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
                .durable_receipt(&local.route.app, &local.route.record)?
                .is_some_and(|receipt| covered(&receipt)));
        }
        let Some((receipt, accepted)) =
            self.durable_receipt_checkpoint(&local.route.app, &local.route.record)?
        else {
            return Ok(false);
        };
        if !covered(&receipt) {
            return Ok(false);
        }
        let history = self.publication_history(&local)?;
        Ok(history
            .get_heads()
            .iter()
            .all(|head| accepted.doc.get_change_by_hash(head).is_some()))
    }

    // A partial incremental's dependencies come only from this actually admitted
    // record. Comparing resulting change hashes distinguishes equal text from
    // genuine causal inclusion, and permits equivalent document/change encodings.
    fn publication_history(&self, local: &LocalDocument) -> Result<automerge::Automerge, String> {
        let current = self
            .core
            .documents
            .get(&local.route)
            .ok_or("retained publication record missing")?;
        let mut history = match local
            .mutation
            .as_ref()
            .ok_or("a sealed publication has no cleartext history")?
        {
            DocumentMutation::Snapshot(bytes) => automerge::Automerge::load(bytes).map_err(err)?,
            DocumentMutation::Incremental(bytes) => {
                let mut history = automerge::Automerge::new();
                history.load_incremental(bytes).map_err(err)?;
                history
            }
        };
        loop {
            let missing = history.get_missing_deps(&[]);
            if missing.is_empty() {
                break;
            }
            let dependencies = missing
                .iter()
                .map(|hash| {
                    current
                        .doc
                        .get_change_by_hash(hash)
                        .ok_or("retained publication dependency is not in admitted history")
                })
                .collect::<Result<Vec<_>, _>>()?;
            history.apply_changes(dependencies).map_err(err)?;
            if history.get_missing_deps(&[]) == missing {
                return Err("retained publication dependencies did not advance".into());
            }
        }
        if !history
            .get_heads()
            .iter()
            .all(|head| current.doc.get_change_by_hash(head).is_some())
        {
            return Err("retained publication changes are not in admitted history".into());
        }
        Ok(history)
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

    fn has_fresh_successor(
        &self,
        original: &LocalDocument,
        outbox: &[NeighbourMsg],
        selected: [u8; 32],
        now: SystemTime,
    ) -> Result<bool, String> {
        let history = self.publication_history(original)?;
        for message in outbox {
            if !crate::outbox::fresh(message, now) {
                continue;
            }
            let candidate = self.retained_document(message, selected)?;
            if candidate.route != original.route
                || candidate.admission.sequence() <= original.admission.sequence()
                || !matches!(candidate.mutation, Some(DocumentMutation::Incremental(_)))
                || self.checkpoint_sequence_covers(message)
            {
                continue;
            }
            let Some(successor) = self.recovery_publication_history(&candidate)? else {
                continue;
            };
            if history
                .get_heads()
                .iter()
                .all(|head| successor.get_change_by_hash(head).is_some())
            {
                return Ok(true);
            }
        }
        Ok(false)
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

    /// The returned prefix is identical to the input; only authorized successors append.
    /// The outer owner must commit this candidate and the entire queue before send.
    ///
    /// # Errors
    ///
    /// Returns an error if a queued message is not authentic retained custody or a successor
    /// cannot be signed.
    pub fn reconcile_local_outbox(
        &mut self,
        outbox: &[NeighbourMsg],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        publication_size(outbox)?;
        let selected = key.verifying_key().to_bytes();
        self.permissions(selected, now)?;
        for original in outbox {
            self.retained_document(original, selected)?;
        }
        let mut candidate = self.clone();
        let mut retained = outbox.to_vec();
        for original in outbox {
            let local = candidate.retained_document(original, selected)?;
            // A sealed write is renewed only through its key holder, by
            // `renew_local_sealed`; it stays in custody until the governor's
            // watermark covers it.
            if local.mutation.is_none()
                || candidate.retained_local_document_retired(original, selected)?
            {
                continue;
            }
            if matches!(local.mutation, Some(DocumentMutation::Incremental(_)))
                && crate::outbox::fresh(original, now)
                && !candidate.checkpoint_sequence_covers(original)
            {
                continue;
            }
            if !candidate.fresh_local_recovery_checkpoint(&local.route, now)?
                || candidate.has_fresh_successor(&local, &retained, selected, now)?
            {
                continue;
            }
            // Validate real history before signing. A Snapshot remains immutable;
            // its complete native history is independently signed as an Incremental.
            candidate.publication_history(&local)?;
            let Some(DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes)) =
                local.mutation
            else {
                continue;
            };
            if retained.len() == MAX_PUBLICATIONS {
                return Err("local publication count limit".into());
            }
            let renewed = candidate.publish_incremental(
                &local.route.app,
                &local.route.record,
                bytes,
                key,
                now,
            )?;
            candidate.retained_document(&renewed, selected)?;
            retained.push(renewed);
            publication_size(&retained)?;
        }
        *self = candidate;
        Ok(retained)
    }

    /// Renew `original`, a sealed write of the selected device still in
    /// custody, once it expired before the governor's watermark retired it.
    /// `reseal` is the channel's key holder: it returns the payload to carry
    /// under the channel's current epoch, or refuses. The renewal keeps the
    /// original's artifact, attestation and class, is signed at `now` under
    /// the author's next sequence, and is admitted as a new local sealed
    /// write. `None` when `original` is not due; on an error nothing changes.
    ///
    /// # Errors
    ///
    /// Returns an error if `original` is not authentic retained custody, the
    /// device may no longer write, `reseal` refuses, or the renewal is not
    /// admitted.
    pub fn renew_local_sealed(
        &mut self,
        original: &NeighbourMsg,
        reseal: &mut Reseal<'_>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Option<NeighbourMsg>, String> {
        let selected = key.verifying_key().to_bytes();
        let local = self.retained_document(original, selected)?;
        if local.mutation.is_some()
            || crate::outbox::fresh(original, now)
            || self.retained_local_document_retired(original, selected)?
        {
            return Ok(None);
        }
        self.permissions(selected, now)?;
        let NeighbourMsg::AppDocument { envelope, .. } = original else {
            return Err("retained local publication requires AppDocument".into());
        };
        let (encryption, payload) = reseal(envelope)?;
        if Some(encryption.channel) != envelope.encryption.map(|sealed| sealed.channel) {
            return Err("a renewal must stay on its write's channel".into());
        }
        let seq = self
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
            envelope.op_class,
            now,
            seq,
            encryption,
        );
        collab_types::sign_op(&mut renewal, key).map_err(err)?;
        let mut candidate = self.clone();
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
        *self = candidate;
        Ok(Some(message))
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

    /// Actual retained peer history; missing original signatures are not reconstructed.
    ///
    /// # Errors
    ///
    /// Returns an error if `history` does not belong to this room or a publication cannot be
    /// signed.
    pub fn recover_local_history(
        &mut self,
        history: &StatePeer,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.recover_local_history_with_outbox(history, &[], key, now)
    }

    /// Recover only history not carried by a fresh exact publication in the actual queue.
    /// Returns new publications only; the supplied queue remains immutable custody.
    ///
    /// # Errors
    ///
    /// Returns an error if `history` or `outbox` does not belong to this room or a
    /// publication cannot be signed.
    pub fn recover_local_history_with_outbox(
        &mut self,
        history: &StatePeer,
        outbox: &[NeighbourMsg],
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        publication_size(outbox)?;
        self.validate_retained_history(history)?;
        let selected = key.verifying_key().to_bytes();
        self.permissions(selected, now)?;
        for message in outbox {
            self.retained_document(message, selected)?;
        }
        let mut candidate = self.clone();
        let mut retained = outbox.to_vec();
        for (route, old) in &history.core.documents {
            if candidate.retained_history_route_retired(route, old)? {
                continue;
            }
            if !candidate.fresh_local_recovery_checkpoint(route, now)? {
                return Err("retained history needs fresh genuine governor checkpoint".into());
            }
            let mut already_carried = false;
            for message in &retained {
                if !crate::outbox::fresh(message, now)
                    || candidate.checkpoint_sequence_covers(message)
                {
                    continue;
                }
                let local = candidate.retained_document(message, selected)?;
                if &local.route != route
                    || local.admission.sequence()
                        <= old
                            .channel
                            .last_seq_per_author
                            .get(&Ed25519PublicKey::from_bytes(selected))
                            .copied()
                            .unwrap_or(0)
                {
                    continue;
                }
                let Some(successor) = candidate.recovery_publication_history(&local)? else {
                    continue;
                };
                if old
                    .doc
                    .base_heads()
                    .iter()
                    .all(|head| successor.get_change_by_hash(head).is_some())
                {
                    already_carried = true;
                    break;
                }
            }
            if already_carried {
                continue;
            }
            if retained.len() == MAX_PUBLICATIONS {
                return Err("local publication count limit".into());
            }
            let bytes = old.doc.clone().save();
            if bytes.len() > MAX_PUBLICATION_BYTES {
                return Err("local publication byte limit".into());
            }
            let message =
                candidate.publish_incremental(&route.app, &route.record, bytes, key, now)?;
            candidate.retained_document(&message, selected)?;
            retained.push(message);
            publication_size(&retained)?;
        }
        *self = candidate;
        Ok(retained.split_off(outbox.len()))
    }

    /// Compare retained real history with the admitted durable governor checkpoint.
    ///
    /// # Errors
    ///
    /// Returns an error if `history` does not belong to this room.
    pub fn retained_local_history_retired(&self, history: &StatePeer) -> Result<bool, String> {
        self.validate_retained_history(history)?;
        for (route, old) in &history.core.documents {
            if !self.retained_history_route_retired(route, old)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

// Count the closed wire DTO before cloning the caller's prefix. These are encoded
// refusal limits, not a native CRDT heap qualification or a durable barrier.
fn publication_size(messages: &[NeighbourMsg]) -> Result<usize, String> {
    struct Count(usize);
    impl postcard::ser_flavors::Flavor for Count {
        type Output = usize;
        fn try_push(&mut self, _: u8) -> postcard::Result<()> {
            self.try_extend(&[0])
        }
        fn try_extend(&mut self, bytes: &[u8]) -> postcard::Result<()> {
            self.0 = self
                .0
                .checked_add(bytes.len())
                .filter(|size| *size <= MAX_PUBLICATION_BYTES)
                .ok_or(postcard::Error::SerializeBufferFull)?;
            Ok(())
        }
        fn finalize(self) -> postcard::Result<usize> {
            Ok(self.0)
        }
    }
    if messages.len() > MAX_PUBLICATIONS {
        return Err("local publication count limit".into());
    }
    let mut total = 0;
    for message in messages {
        total = postcard::serialize_with_flavor(message, Count(total))
            .map_err(|_| "local publication byte limit")?;
    }
    Ok(total)
}

#[cfg(test)]
mod sealed_renewal_tests;
#[cfg(test)]
mod tests;

mod projection;
pub use projection::LocalRecoveryReservation;
