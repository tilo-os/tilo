//! Local immutable publication custody: the signed frames this device still
//! owes the workspace, and when the workspace has retired them.
//! Nothing here admits remote historical wire data or commits an owner's storage.
use super::{
    AdmissionError, DocumentAdmission, DocumentMutation, NeighbourMsg, Route, SignedOp, SigningKey,
    StatePeer, SystemTime, err, peer_handle,
};
use crate::state::facades::{PeerOutbox, PeerOutboxMut};

/// A channel key holder's renewal of a sealed write: the encryption and
/// payload a renewal carries under the channel's current epoch, or a refusal.
pub type Reseal<'a> =
    dyn FnMut(&SignedOp) -> Result<(collab_types::EncryptionMetadata, Vec<u8>), String> + 'a;

struct LocalDocument {
    admission: DocumentAdmission,
    route: Route,
    /// `None` for a sealed write: this replica holds only its ciphertext, so
    /// it has no history to check, only the channel's key holder can renew it,
    /// and only a durable receipt naming it retires it.
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
        if cluster_id != &self.core.workspace
            || envelope.author.as_bytes() != &selected
            || *peer != peer_handle(&selected)
        {
            return Err("retained local publication selected author or workspace mismatch".into());
        }
        collab_types::verify_op(envelope).map_err(err)?;
        let route = Route {
            app: app_id.clone(),
            record: record_id.clone(),
        };
        let Some(document) = self.core.documents.get(&route) else {
            return Err(format!(
                "retained local publication route {}/{} is not admitted",
                route.app, route.record
            )
            .into());
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
    /// what a receipt has to cover before it retires the frame (see
    /// [`history`]).
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
    /// Whether the workspace this replica holds retired the write:
    /// a participant or store refused that write of its author on its record,
    /// or holds a durable receipt that names it. Either is the workspace's
    /// answer that the frame is spent, and only those.
    pub fn retained_local_document_retired(
        self,
        message: &NeighbourMsg,
        selected: [u8; 32],
    ) -> Result<bool, String> {
        let local = self.peer.retained_document(message, selected)?;
        if self
            .peer
            .workspace()
            .write_refusals()
            .into_iter()
            .any(|refusal| {
                refusal.workspace == self.peer.core.workspace
                    && refusal.app == local.route.app
                    && refusal.record == local.route.record
                    && refusal.author == local.admission.author()
                    && refusal.seq >= local.admission.sequence()
            })
        {
            return Ok(true);
        }
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
        self.peer
            .permissions(selected, now)
            .map_err(|error| error.to_string())?;
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
            cluster_id: candidate.core.workspace.clone(),
            record_id: local.route.record,
            envelope: Box::new(renewal),
        };
        candidate.retained_document(&message, selected)?;
        *self.peer = candidate;
        Ok(Some(message))
    }
}

#[cfg(test)]
mod sealed_renewal_tests;
#[cfg(test)]
mod tests;

mod history;
