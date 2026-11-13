//! Local exact retries use the signed author, never an invented carrier origin.

use super::{AdmissionError, DocumentAdmission, Route, SignedOp, StatePeer, SystemTime};

impl StatePeer {
    /// Look up an already recorded local envelope with current authority,
    /// signature and route validation. The caller must independently require
    /// that its selected local device is exactly `envelope.author`.
    pub fn lookup_exact_local_document(
        &self,
        app: &str,
        record: &str,
        envelope: &SignedOp,
        now: SystemTime,
    ) -> Result<Option<DocumentAdmission>, AdmissionError> {
        self.lookup_exact_document_parts(
            *envelope.author.as_bytes(),
            app,
            &self.core.workspace,
            record,
            envelope,
            now,
        )
    }

    /// `envelope`, this device's own write to `app`'s `record`, as the record
    /// takes it next, and whether it is to be signed again. A device has several
    /// local writers (each instance of an app, the file daemon), and each signs
    /// its writes with the device's key before its owner's turn orders them: a
    /// write can reach the turn after a write another writer signed later took a
    /// greater sequence, and the record would refuse it as a replay. Such a write
    /// is drafted again under a sequence past the record's floor, for the device
    /// to sign. A write the record holds already and one still past the floor
    /// stay as they are.
    ///
    /// # Errors
    ///
    /// The record's exact admissions cannot be read.
    pub fn sequenced_local_document(
        &self,
        app: &str,
        record: &str,
        mut envelope: SignedOp,
        now: SystemTime,
    ) -> Result<(SignedOp, bool), String> {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let Some(floor) = self
            .core
            .documents
            .get(&route)
            .and_then(|document| document.channel.last_seq_per_author.get(&envelope.author))
            .copied()
            .filter(|floor| envelope.seq <= *floor)
        else {
            return Ok((envelope, false));
        };
        // A sequence another write of this device took (a compaction it made since) is numbered
        // past, as any it is under.
        match self.lookup_exact_local_document(app, record, &envelope, now) {
            Ok(Some(_)) => return Ok((envelope, false)),
            Ok(None) | Err(super::AdmissionError::ConflictingSequence) => {}
            Err(error) => return Err(error.to_string()),
        }
        envelope.seq = collab_types::next_unordered_document_sequence(now)
            .max(floor.checked_add(1).ok_or("sequence exhausted")?);
        Ok((envelope, true))
    }

    /// `frame`, an encoded [`collab_types::DocumentFrame`] of `record` of `app`, drafted as
    /// `author`'s next write of it after the `drafted` writes of it drafted before in the same
    /// owner turn, unsigned: sealed for the record's channel by `seal`, the
    /// channel's key holder, with the claims of its changes for readers without the key, or in
    /// the clear where `seal` is `None`. A write in the clear that starts a record is refused
    /// when the workspace does not let `author` create it or holds it already.
    ///
    /// # Errors
    ///
    /// The frame does not decode, sealing failed, or the record refuses the write.
    pub fn draft_write(
        &self,
        (app, record): (&str, &str),
        frame: &[u8],
        seal: Option<&mut super::SealCheckpoint<'_>>,
        author: [u8; 32],
        drafted: u64,
        now: SystemTime,
    ) -> Result<SignedOp, String> {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let mutation =
            match collab_types::DocumentFrame::decode(frame).map_err(|error| error.to_string())? {
                collab_types::DocumentFrame::Whole(mutation) => Some(mutation),
                collab_types::DocumentFrame::Segment(_) => None,
            };
        let author = collab_types::Ed25519PublicKey::from_bytes(author);
        let document = self.core.documents.get(&route);
        let seq = document
            .and_then(|held| held.channel.last_seq_per_author.get(&author).copied())
            .unwrap_or(0)
            .checked_add(1)
            .and_then(|next| next.checked_add(drafted))
            .ok_or("sequence exhausted")?;
        let artifact = collab_types::document_artifact_id(app, &self.core.workspace, record);
        let attestation = collab_types::document_attestation_id(app, &self.core.workspace, record);
        let surface = self.signed_surface(app);
        let Some(seal) = seal else {
            // A device that starts a record is told now when the workspace does not let it
            // create one, or when the record is there already: a peer's copy of the same start
            // is held or merged as the nothing it is, but its own write would be answered as
            // published while no peer ever sees it.
            if matches!(&mutation, Some(collab_types::DocumentMutation::Snapshot(bytes)) if super::empty_genesis(bytes))
                && !self.signed_by_owner(author.as_bytes())
            {
                if document.is_some() {
                    return Err("the record exists already".into());
                }
                if !self.may_create_record(&self.membership(*author.as_bytes(), now)?, &route, now)
                {
                    return Err("the workspace does not let this device create the record".into());
                }
            }
            return Ok(SignedOp::new_unsigned(
                frame.to_vec(),
                artifact,
                author,
                attestation,
                collab_types::OpKind::Change,
                now,
                seq,
            )
            .with_surface(surface));
        };
        // A change needs the record it changes, held or drafted before it; a checkpoint is the
        // whole record, so it is all a route it founds needs.
        let retention = collab_types::ContentRetention::of_frame(frame);
        if document.is_none() && drafted == 0 && retention == collab_types::ContentRetention::Delta
        {
            return Err("sealed write to a record this state does not hold".into());
        }
        // Readers without the key decide a sealed write by the claims of its changes, as a
        // member's page names them: each naming the units its action may write by the surface
        // the op names.
        let claims = mutation
            .as_ref()
            .and_then(|mutation| {
                tilo_failure::ignored!(
                    "a segment of a frame, or changes that name no action, claim nothing",
                    collab_types::mutation_claims(mutation, collab_types::record_unit).ok()
                )
            })
            .map(|claims| {
                self.surfaces.with_footprints(
                    &collab_types::Record {
                        room: &self.core.workspace,
                        app,
                        record,
                    },
                    document
                        .and_then(|held| super::record_kind(&*held.doc))
                        .as_deref(),
                    surface.as_ref(),
                    claims,
                )
            })
            .unwrap_or_default();
        let (metadata, ciphertext) = seal(frame)?;
        if metadata.retention() != Some(retention)
            || metadata.channel
                != collab_types::document_channel_id(app, &self.core.workspace, record)
        {
            return Err("a sealed write is a delta or a checkpoint of the record's channel".into());
        }
        Ok(SignedOp::new_unsigned_with_encryption(
            ciphertext,
            artifact,
            author,
            attestation,
            collab_types::OpKind::Change,
            now,
            seq,
            metadata,
        )
        .with_claims(claims)
        .with_surface(surface))
    }

    /// Admit `envelope`, a local app's signed write to `record`, as a new
    /// exact admission: refused unless it is admitted now, completely and
    /// not as a duplicate, with evidence of it in the exact admission index.
    /// An owner admits into a candidate and discards it on an error.
    ///
    /// # Errors
    ///
    /// Returns a message when the write is refused, deferred, incomplete or
    /// rejected (`not newly admitted`), when its signed sequence is at or
    /// below the author's replay floor on the route (`signed sequence is
    /// stale`), or when it leaves no exact admission evidence. None of these
    /// waits for an asynchronous receipt: the evidence is the exact admission
    /// index entry written by this same synchronous call.
    pub fn admit_new_local_document(
        &mut self,
        app: &str,
        record: &str,
        envelope: SignedOp,
        now: SystemTime,
    ) -> Result<(), String> {
        // The evidence is the entry this admission writes, found by its key:
        // comparing the index with a copy of it from before made every write
        // cost every entry the index held since it last settled.
        let evidence = DocumentAdmission::from_op(&self.core.workspace, app, record, &envelope)
            .map_err(|error| error.to_string())?;
        let held = self.core.exact_admissions.get(&evidence.key).is_some()
            || self.core.exact_admissions.covers(&evidence.key);
        let report = self.admit_local(app, record, envelope, now)?;
        let outcome = &report.summary;
        if outcome.duplicate {
            return Err("native local document signed sequence is stale".into());
        }
        if outcome.deferred || outcome.incomplete.is_some() || outcome.rejected != 0 {
            let waiting = match &report.primary {
                super::InputDisposition::RetryLater(prerequisite) => {
                    format!(", waiting for {prerequisite}")
                }
                _ => String::new(),
            };
            let incomplete = outcome
                .incomplete
                .as_deref()
                .map_or_else(String::new, |reason| format!(" ({reason})"));
            return Err(format!(
                "native local document was deferred, incomplete or rejected, not newly admitted{waiting}{incomplete}"
            ));
        }
        if held
            || !self.core.admission_v5
            || self.core.exact_admissions.get(&evidence.key) != Some(&evidence.operation_id)
        {
            return Err("native local document has no exact admission evidence".into());
        }
        Ok(())
    }

    /// Take `envelope`, this device's own write to `app`'s `record`, back out of the record:
    /// the workspace refused it, so the record holds what it would without it. A sealed write
    /// leaves the record's sealed history; a write in the clear takes the changes it brought
    /// out of the document, with every change of this replica built on them.
    ///
    /// # Errors
    /// The document left holds changes that no longer apply.
    pub fn withdraw_local_document(
        &mut self,
        app: &str,
        record: &str,
        envelope: &SignedOp,
    ) -> Result<(), String> {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let Some(document) = self.core.documents.get(&route) else {
            return Ok(());
        };
        // The write was never the workspace's: no evidence of it stays, so a read of the record
        // does not wait for it.
        self.core
            .exact_admissions
            .withdraw(&crate::admission::AdmissionKey {
                workspace: self.core.workspace.clone(),
                app: app.into(),
                record: record.into(),
                author: *envelope.author.as_bytes(),
                sequence: envelope.seq,
            })
            .map_err(|error| error.to_string())?;
        if document
            .sealed
            .iter()
            .any(|held| held.signature == envelope.signature)
        {
            let kept: Vec<SignedOp> = document
                .sealed
                .iter()
                .filter(|held| held.signature != envelope.signature)
                .cloned()
                .collect();
            // The author's floor falls back to its last write the record still holds.
            let floor = kept
                .iter()
                .filter(|held| held.author == envelope.author)
                .map(|held| held.seq)
                .max();
            if let Some(document) = self.core.documents.get_mut(&route) {
                let watermarks = &mut document.channel.last_seq_per_author;
                if watermarks.get(&envelope.author) == Some(&envelope.seq) {
                    match floor {
                        Some(floor) => {
                            watermarks.insert(envelope.author, floor);
                        }
                        None => {
                            watermarks.remove(&envelope.author);
                        }
                    }
                }
                document.sealed = kept.into();
            }
            return Ok(());
        }
        let mut withdrawn: std::collections::BTreeSet<automerge::ChangeHash> = envelope
            .claims
            .iter()
            .map(|claim| claim.change)
            .chain(super::personal::change_hashes(&envelope.payload))
            .map(automerge::ChangeHash)
            .collect();
        let mut doc = (*document.doc).clone();
        if !withdrawn
            .iter()
            .any(|hash| doc.get_change_meta_by_hash(hash).is_some())
        {
            return Ok(());
        }
        let mut kept = Vec::new();
        for change in doc.get_changes(&[]) {
            if withdrawn.contains(&change.hash())
                || change.deps().iter().any(|dep| withdrawn.contains(dep))
            {
                withdrawn.insert(change.hash());
            } else {
                kept.push(change);
            }
        }
        let mut rebuilt = automerge::AutoCommit::new();
        tilo_automerge::ApplyIncoming::apply_incoming(&mut rebuilt, kept)
            .map_err(|error| error.to_string())?;
        if let Some(document) = self.core.documents.get_mut(&route) {
            document.doc = rebuilt.into();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NeighbourMsg;
    use collab_types::Ed25519PublicKey;
    use ed25519_dalek::SigningKey;
    use tilo_user_identity::DeviceCert;
    use web_time::Duration;

    fn admitted() -> (StatePeer, SignedOp, SigningKey, DeviceCert, SystemTime) {
        let (mut peer, key, other, certificate, now) = crate::admission_tests::seeded();
        peer.select_admission_v5().unwrap();
        let current_document = peer.document("test.app", "tab:1").unwrap();
        let NeighbourMsg::AppDocument { envelope, .. } = peer
            .publication_mut()
            .publish_document("test.app", "tab:1", current_document, &key, now)
            .unwrap()
        else {
            panic!("expected actual signed document publication")
        };
        (peer, *envelope, other, certificate, now)
    }

    #[test]
    fn a_new_local_write_is_admitted_once_with_exact_evidence() {
        let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
        peer.select_admission_v5().unwrap();
        let bytes = peer.document("test.app", "tab:1").unwrap();
        let NeighbourMsg::AppDocument { envelope, .. } = peer
            .clone()
            .publication_mut()
            .publish_document("test.app", "tab:1", bytes, &key, now)
            .unwrap()
        else {
            panic!("expected actual signed document publication")
        };
        peer.admit_new_local_document("test.app", "tab:1", (*envelope).clone(), now)
            .unwrap();
        assert!(
            peer.lookup_exact_local_document("test.app", "tab:1", &envelope, now)
                .unwrap()
                .is_some()
        );
        assert!(
            peer.clone()
                .admit_new_local_document("test.app", "tab:1", *envelope, now)
                .is_err(),
            "an exact retry is not a new admission"
        );
    }

    #[test]
    fn local_exact_retry_recognizes_recorded_envelope_without_mutation_or_freshness_reset() {
        let (peer, envelope, _, _, now) = admitted();
        let expected =
            DocumentAdmission::from_op(peer.workspace_id(), "test.app", "tab:1", &envelope)
                .unwrap();
        let before = peer.durable().unwrap();
        for time in [now, now + Duration::from_secs(3600)] {
            assert_eq!(
                peer.lookup_exact_local_document("test.app", "tab:1", &envelope, time)
                    .unwrap(),
                Some(expected.clone())
            );
        }
        assert_eq!(peer.durable().unwrap(), before);
    }

    #[test]
    fn local_exact_retry_rejects_wrong_route_and_tampered_identity() {
        let (peer, envelope, _, other, now) = admitted();
        assert!(
            peer.lookup_exact_local_document("other.app", "tab:1", &envelope, now)
                .is_err()
        );
        assert!(
            peer.lookup_exact_local_document("test.app", "tab:2", &envelope, now)
                .is_err()
        );
        let mut tampered = envelope;
        tampered.author = Ed25519PublicKey::from_bytes(other.device_public_key);
        assert!(
            peer.lookup_exact_local_document("test.app", "tab:1", &tampered, now)
                .is_err()
        );
    }

    #[test]
    fn local_exact_retry_does_not_reuse_another_valid_signers_evidence() {
        let (peer, mut envelope, key, other, now) = admitted();
        envelope.author = Ed25519PublicKey::from_bytes(other.device_public_key);
        collab_types::sign_op(&mut envelope, &key).unwrap();
        assert_eq!(
            peer.lookup_exact_local_document("test.app", "tab:1", &envelope, now)
                .unwrap(),
            None
        );
    }

    #[test]
    fn replay_floor_without_exact_entry_requires_a_fresh_signed_successor() {
        use automerge::transaction::Transactable as _;
        let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
        let original = peer.document("test.app", "tab:1").unwrap();
        let NeighbourMsg::AppDocument {
            envelope: first, ..
        } = peer
            .clone()
            .publication_mut()
            .publish_document("test.app", "tab:1", original.clone(), &key, now)
            .unwrap()
        else {
            panic!("first signed snapshot")
        };
        let mut changed = automerge::AutoCommit::load(&original).unwrap();
        changed
            .put(automerge::ROOT, "diagnostic", "different")
            .unwrap();
        let NeighbourMsg::AppDocument {
            envelope: stale, ..
        } = peer
            .clone()
            .publication_mut()
            .publish_document("test.app", "tab:1", changed.save(), &key, now)
            .unwrap()
        else {
            panic!("stale signed snapshot")
        };
        assert_eq!(first.seq, stale.seq);
        let first_seq = first.seq;
        peer.admit_local_app_document("test.app", "tab:1", *first, now)
            .unwrap();
        peer.select_admission_v5().unwrap();
        assert!(
            peer.lookup_exact_local_document("test.app", "tab:1", &stale, now)
                .unwrap()
                .is_none()
        );
        let before = peer.durable().unwrap();
        assert_eq!(
            peer.admit_new_local_document("test.app", "tab:1", *stale, now)
                .unwrap_err(),
            "native local document signed sequence is stale"
        );
        assert_eq!(peer.durable().unwrap(), before);

        let NeighbourMsg::AppDocument {
            envelope: fresh, ..
        } = peer
            .clone()
            .publication_mut()
            .publish_document("test.app", "tab:1", changed.save(), &key, now)
            .unwrap()
        else {
            panic!("fresh signed snapshot")
        };
        assert_eq!(fresh.seq, first_seq + 1);
        peer.admit_new_local_document("test.app", "tab:1", *fresh, now)
            .unwrap();
    }
}
