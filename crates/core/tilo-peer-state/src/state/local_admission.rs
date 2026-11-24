//! Local exact retries use the signed author, never an invented carrier origin.

use super::{
    AdmissionError, DocumentAdmission, MissingPrerequisite, Route, SignedOp, StatePeer, SystemTime,
};

/// Why this device's own write is absent from its document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotAdmitted {
    /// Kept: the workspace has not admitted what it waits for here yet, and
    /// the write is admitted once it is.
    Waiting(MissingPrerequisite),
    /// Why the document refused the write, as its author is told it, and the refusing device's
    /// own words.
    Refused {
        reason: neighbour_wire::RefusalReason,
        detail: String,
    },
    /// The sealed history of the record has no room for the write: it waits for a device holding
    /// the record's keys to compact it.
    HistoryFull,
    /// The workspace's rules refused the change before it went: the document it writes, the
    /// changes it carries and why, as a server's refusal of a change names them.
    ChangeRefused(Box<crate::ChangeRefused>),
    /// The record's stored history was set aside when this device opened the workspace, and it
    /// is asked of another device again: nothing written to it now could be kept, so the write is
    /// refused until the history is back ([`super::StatePeer::quarantined`]).
    Restoring,
}

impl std::fmt::Display for NotAdmitted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Waiting(prerequisite) => write!(f, "the write waits for {prerequisite}"),
            Self::Refused { detail, .. } => f.write_str(detail),
            Self::HistoryFull => AdmissionError::HistoryFull.fmt(f),
            Self::Restoring => f.write_str("This record is being restored from another device."),
            Self::ChangeRefused(refused) => f.write_str(&refused.reason),
        }
    }
}

impl From<AdmissionError> for NotAdmitted {
    fn from(error: AdmissionError) -> Self {
        match error {
            AdmissionError::ChangeRefused(refused) => Self::ChangeRefused(refused),
            AdmissionError::HistoryFull => Self::HistoryFull,
            other => Self::Refused {
                reason: other.reason(),
                detail: other.to_string(),
            },
        }
    }
}

impl From<crate::MembershipError> for NotAdmitted {
    fn from(error: crate::MembershipError) -> Self {
        AdmissionError::from(error).into()
    }
}

impl From<NotAdmitted> for String {
    fn from(refusal: NotAdmitted) -> Self {
        refusal.to_string()
    }
}

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
        // A device its person removed looks up nothing of its own: it writes
        // nothing more, whatever of its past the workspace keeps.
        if matches!(
            self.permissions(*envelope.author.as_bytes(), now),
            Err(super::MembershipError::Revoked)
        ) {
            return Err(super::MembershipError::Revoked.into());
        }
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
        // past, as any it is under. A write drafted in this turn and not signed yet (a write of
        // this device came in meanwhile: one of the device's other connections sent it) is a
        // copy of nothing admitted: it is numbered past and signed then.
        if collab_types::verify_op(&envelope).is_ok() {
            match self.lookup_exact_local_document(app, record, &envelope, now) {
                Ok(Some(_)) => return Ok((envelope, false)),
                Ok(None) | Err(super::AdmissionError::ConflictingSequence) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        envelope.seq = collab_types::next_unordered_document_sequence(now)
            .max(floor.checked_add(1).ok_or("sequence exhausted")?);
        Ok((envelope, true))
    }

    /// `envelope`, a sealed write to a resource store's document drafted by
    /// [`Self::draft_write`] and not signed yet, with each claim of a change that edits its
    /// records whole naming the kinds `kinds` holds for it: what the store that wrote it reads
    /// from the records, which this peer, holding only the sealed write, cannot. Each claim
    /// may then write what the surface of each of its kinds lets it.
    pub fn name_record_kinds(
        &self,
        (app, record): (&str, &str),
        envelope: &mut SignedOp,
        kinds: &collab_types::claim::RecordKinds,
    ) {
        let mut claims = std::mem::take(&mut envelope.claims);
        collab_types::claim::name_record_kinds(&mut claims, kinds);
        envelope.claims = self.footprints((app, record), claims);
    }

    /// `claims` of a write to `record` of `app`, each with the units its action may write by the
    /// surface it is decided against.
    fn footprints(
        &self,
        (app, record): (&str, &str),
        claims: Vec<collab_types::ChangeClaim>,
    ) -> Vec<collab_types::ChangeClaim> {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        self.surfaces.with_footprints(
            &collab_types::WrittenDocument {
                workspace: &self.core.workspace,
                app,
                document: record,
            },
            self.core
                .documents
                .get(&route)
                .and_then(|held| super::resource_type(&*held.doc))
                .as_deref(),
            self.signed_surface(app).as_ref(),
            claims,
        )
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
        route: (&str, &str),
        readers: Option<&str>,
        frame: &[u8],
        seal: Option<&mut super::SealCheckpoint<'_>>,
        author: [u8; 32],
        drafted: u64,
        now: SystemTime,
    ) -> Result<SignedOp, String> {
        self.draft_checked_write(route, readers, frame, seal, author, drafted, now)
            .map_err(|refusal| refusal.to_string())
    }

    /// [`Self::draft_write`], telling a sealed write the workspace's rules refuse
    /// ([`DraftRefusal::Change`]) from one that could not be drafted at all.
    ///
    /// A sealed write is decided here as every device that opens it decides it
    /// ([`Self::check_opened_write`]): a device without the key cannot, and a
    /// write every reader would leave out is better never sent.
    ///
    /// # Errors
    ///
    /// As [`Self::draft_write`].
    pub(crate) fn draft_checked_write(
        &self,
        (app, record): (&str, &str),
        readers: Option<&str>,
        frame: &[u8],
        seal: Option<&mut super::SealCheckpoint<'_>>,
        author: [u8; 32],
        drafted: u64,
        now: SystemTime,
    ) -> Result<SignedOp, DraftRefusal> {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let mutation = match collab_types::DocumentFrame::decode(frame)
            .map_err(|error| DraftRefusal::Refused(error.to_string()))?
        {
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
            .ok_or_else(|| DraftRefusal::Refused("sequence exhausted".into()))?;
        let artifact = collab_types::document_artifact_id(app, &self.core.workspace, record);
        let attestation = collab_types::document_attestation_id(app, &self.core.workspace, record);
        let surface = self.signed_surface(app);
        let Some(seal) = seal else {
            self.check_document_key_scope((app, record), readers, None, now, false)
                .map_err(DraftRefusal::Refused)?;
            // A device that starts a record is told now when the workspace does not let it
            // create one, or when the record is there already: a peer's copy of the same start
            // is held or merged as the nothing it is, but its own write would be answered as
            // published while no peer ever sees it.
            if matches!(&mutation, Some(collab_types::DocumentMutation::Snapshot(bytes)) if super::empty_genesis(bytes))
                && !self.signed_by_governor(author.as_bytes())
            {
                if document.is_some() {
                    return Err(DraftRefusal::Refused("the record exists already".into()));
                }
                if !self.may_create_document(
                    &self
                        .membership(*author.as_bytes(), now)
                        .map_err(DraftRefusal::Refused)?,
                    &route,
                    now,
                ) {
                    return Err(DraftRefusal::Refused(
                        "the workspace does not let this device create the record".into(),
                    ));
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
            .with_surface(surface)
            .with_governance(&self.governance_heads().map_err(DraftRefusal::Refused)?));
        };
        // A change needs the record it changes, held or drafted before it; a checkpoint is the
        // whole record, so it is all a route it founds needs.
        let retention = collab_types::ContentRetention::of_frame(frame);
        if document.is_none() && drafted == 0 && retention == collab_types::ContentRetention::Delta
        {
            return Err(DraftRefusal::Refused(
                "sealed write to a record this state does not hold".into(),
            ));
        }
        let claims = mutation
            .as_ref()
            .map(|mutation| self.sealed_claims((app, record), mutation))
            .unwrap_or_default();
        let (metadata, ciphertext) = seal(frame).map_err(DraftRefusal::Refused)?;
        self.check_document_key_scope((app, record), readers, Some(metadata), now, false)
            .map_err(DraftRefusal::Refused)?;
        if metadata.retention() != Some(retention) {
            return Err(DraftRefusal::Refused(
                "a sealed write is a delta or a checkpoint of the record".into(),
            ));
        }
        let op = SignedOp::new_unsigned_with_encryption(
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
        .with_surface(surface)
        .with_governance(&self.governance_heads().map_err(DraftRefusal::Refused)?);
        // A write whose segments are still to come is decided by its readers once it is whole;
        // one written whole is decided now. A write the workspace has not decided the rules for
        // yet (they are still arriving here) goes, and its readers decide it.
        if let Some(mutation) = &mutation
            && let Err(super::document_capture::OpenedRefusal::Refused(refused)) =
                self.check_opened_write((app, record), &op, mutation)
        {
            return Err(DraftRefusal::Change(refused));
        }
        Ok(op)
    }

    /// What a sealed write of `mutation` to `record` of `app` claims: readers without the key
    /// decide the write by the claims of its changes, as a member's page names them, each naming
    /// the units its action may write by the app interface the write names. Changes that name no
    /// action claim nothing.
    pub(crate) fn sealed_claims(
        &self,
        (app, record): (&str, &str),
        mutation: &collab_types::DocumentMutation,
    ) -> Vec<collab_types::ChangeClaim> {
        let Some(claims) = tilo_failure::ignored!(
            "changes that name no action claim nothing",
            collab_admission::mutation_claims(mutation, collab_types::record_unit).ok()
        ) else {
            return Vec::new();
        };
        self.footprints((app, record), claims)
    }

    /// Admit `envelope`, a local app's signed write to `record`, as a new
    /// exact admission: refused unless it is admitted now, completely and
    /// not as a duplicate, with evidence of it in the exact admission index.
    /// An owner admits into a candidate and discards it on an error.
    ///
    /// # Errors
    ///
    /// [`NotAdmitted::Waiting`] when the workspace has not admitted what the
    /// write needs here yet, and [`NotAdmitted::Refused`] when it does not
    /// take it, its signed sequence is at or below the author's replay floor
    /// on the route, or it leaves no exact admission evidence. Neither waits
    /// for an asynchronous receipt: the evidence is the exact admission index
    /// entry written by this same synchronous call.
    pub fn admit_new_local_document(
        &mut self,
        app: &str,
        document: &str,
        envelope: SignedOp,
        now: SystemTime,
    ) -> Result<(), NotAdmitted> {
        self.admit_new_local_document_read_by((app, document), None, envelope, now)
    }

    /// Admit a local store write with the reader set bound to its route: what a holder writes
    /// through when it keeps a document for some of a workspace's members alone.
    pub fn admit_new_local_document_read_by(
        &mut self,
        (app, document): (&str, &str),
        readers: Option<&str>,
        envelope: SignedOp,
        now: SystemTime,
    ) -> Result<(), NotAdmitted> {
        // The evidence is the entry this admission writes, found by its key:
        // comparing the index with a copy of it from before made every write
        // cost every entry the index held since it last settled.
        let evidence = DocumentAdmission::from_op(&self.core.workspace, app, document, &envelope)
            .map_err(NotAdmitted::from)?;
        // A compaction takes the number of the last write of its author it
        // folds, so that number is held already; what is new is the compaction
        // itself, and its evidence is the mark its fold leaves.
        let compaction = envelope.kind == collab_types::OpKind::Compaction;
        let held = !compaction
            && (self.core.exact_admissions.get(&evidence.key).is_some()
                || self.core.exact_admissions.covers(&evidence.key));
        let report = self.admit_local((app, document), readers, envelope, now)?;
        let outcome = &report.summary;
        if outcome.duplicate {
            return Err(NotAdmitted::Refused {
                reason: neighbour_wire::RefusalReason::Conflict,
                detail: "native local document signed sequence is stale".into(),
            });
        }
        if outcome.deferred || outcome.incomplete.is_some() || outcome.rejected != 0 {
            let incomplete = outcome
                .incomplete
                .as_deref()
                .map_or_else(String::new, |reason| format!(" ({reason})"));
            return Err(match &report.primary {
                super::InputDisposition::RetryLater(prerequisite) => {
                    NotAdmitted::Waiting(prerequisite.clone())
                }
                _ => NotAdmitted::Refused {
                    reason: neighbour_wire::RefusalReason::Malformed,
                    detail: format!(
                        "native local document was incomplete or rejected, not newly admitted{incomplete}"
                    ),
                },
            });
        }
        let settled = self.core.exact_admissions.get(&evidence.key) == Some(&evidence.operation_id)
            || (compaction && self.core.exact_admissions.covers(&evidence.key));
        if held || !self.core.exact_selected || !settled {
            return Err(NotAdmitted::Refused {
                reason: neighbour_wire::RefusalReason::Malformed,
                detail: "native local document has no exact admission evidence".into(),
            });
        }
        Ok(())
    }

    /// Take `write`, this device's, out of the document's sealed history: the renewal that
    /// takes its place carries the same change, so the document holds one of the two. What its
    /// admission decided stays decided.
    pub fn replace_local_sealed(&mut self, app: &str, record: &str, write: &SignedOp) {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let Some(document) = self.core.documents.get_mut(&route) else {
            return;
        };
        if document
            .sealed
            .iter()
            .all(|held| held.signature != write.signature)
        {
            return;
        }
        document.sealed = document
            .sealed
            .iter()
            .filter(|held| held.signature != write.signature)
            .cloned()
            .collect::<Vec<_>>()
            .into();
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
        let key = crate::admission::AdmissionKey {
            workspace: self.core.workspace.clone(),
            app: app.into(),
            record: record.into(),
            author: *envelope.author.as_bytes(),
            sequence: envelope.seq,
        };
        // The write was never the workspace's: no evidence of it stays, so a read of the record
        // does not wait for it.
        self.core
            .exact_admissions
            .withdraw(&key)
            .map_err(|error| error.to_string())?;
        // Every write of the author a compaction folded is decided, whatever its bytes.
        let folded = self.core.exact_admissions.mark(&key);
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
            // The author's floor falls back to its last write the record still holds, never
            // below the writes a compaction folded: a write numbered under them would be one the
            // record decided already, and the author's next write could never be taken.
            let floor = kept
                .iter()
                .filter(|held| held.author == envelope.author)
                .map(|held| held.seq)
                .chain(folded)
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
            .chain(super::change_hashes::change_hashes(&envelope.payload))
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

/// Why a write could not be drafted ([`StatePeer::draft_checked_write`]).
#[derive(Debug)]
pub(crate) enum DraftRefusal {
    /// The workspace's rules refuse the sealed write, as every device that
    /// opens it would: named as a received change's refusal is.
    Change(Box<crate::ChangeRefused>),
    /// It could not be drafted: its frame, its seal or the record refuse it.
    Refused(String),
}

impl std::fmt::Display for DraftRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Change(refused) => write!(f, "{refused}"),
            Self::Refused(reason) => f.write_str(reason),
        }
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
        peer.select_exact_admissions().unwrap();
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

    /// A local write whose app surface this process does not hold waits for
    /// it: it is kept as it is, not taken for a write the workspace refuses.
    #[test]
    fn a_local_write_that_waits_for_its_app_surface_is_kept() {
        use collab_types::Surfaces;
        use collab_types::surface::ContentSurfaces;
        // No surface this process has is the one the write names.
        crate::decide_with_surfaces(
            Surfaces::from_source(std::sync::Arc::new(ContentSurfaces::new(
                |_: &collab_types::WrittenDocument<'_>, _: &str| -> Option<Vec<u8>> { None },
            ))),
            |_| None,
        );
        let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
        peer.select_exact_admissions().unwrap();
        let bytes = peer.document("test.app", "tab:1").unwrap();
        let NeighbourMsg::AppDocument { envelope, .. } = peer
            .clone()
            .publication_mut()
            .publish_document("test.app", "tab:1", bytes, &key, now)
            .unwrap()
        else {
            panic!("expected actual signed document publication")
        };
        // The surface is signed with the rest of the op, as the broker names it before signing.
        let mut named = envelope.with_surface(Some([0x42; 32]));
        collab_types::sign_op(&mut named, &key).unwrap();
        let before = peer.document("test.app", "tab:1").unwrap();
        assert_eq!(
            peer.admit_new_local_document("test.app", "tab:1", named, now)
                .unwrap_err(),
            NotAdmitted::Waiting(MissingPrerequisite::AppSurface)
        );
        assert_eq!(
            peer.document("test.app", "tab:1").unwrap(),
            before,
            "a write that waits leaves the record as it was"
        );
    }

    tilo_test_env::scenario! {
        fn a_local_write_is_admitted_with_exact_evidence_or_a_fresh_successor;

        phase a_new_local_write_is_admitted_once_with_exact_evidence;
        {
            let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
            peer.select_exact_admissions().unwrap();
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

        phase replay_floor_without_exact_entry_requires_a_fresh_signed_successor;
        {
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
            peer.select_exact_admissions().unwrap();
            assert!(
                peer.lookup_exact_local_document("test.app", "tab:1", &stale, now)
                    .unwrap()
                    .is_none()
            );
            let before = peer.durable().unwrap();
            assert_eq!(
                peer.admit_new_local_document("test.app", "tab:1", *stale, now)
                    .unwrap_err(),
                NotAdmitted::Refused {
                    reason: neighbour_wire::RefusalReason::Conflict,
                    detail: "native local document signed sequence is stale".into(),
                }
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

    /// One admitted write, looked up again: only its own envelope on its own
    /// route is recognized, however much later, and no lookup changes the
    /// state or resets its freshness.
    #[test]
    fn local_exact_retry_recognizes_only_the_recorded_envelope_on_its_own_route() {
        let (peer, envelope, other_key, other, now) = admitted();
        let expected =
            DocumentAdmission::from_op(peer.workspace_id(), "test.app", "tab:1", &envelope)
                .unwrap();
        let before = peer.durable().unwrap();
        for time in [now, now + Duration::from_secs(3600)] {
            assert_eq!(
                peer.lookup_exact_local_document("test.app", "tab:1", &envelope, time)
                    .unwrap(),
                Some(expected.clone()),
                "the recorded envelope at {time:?}"
            );
        }
        for (case, app, record) in [
            ("another app", "other.app", "tab:1"),
            ("another record", "test.app", "tab:2"),
        ] {
            assert!(
                peer.lookup_exact_local_document(app, record, &envelope, now)
                    .is_err(),
                "{case}"
            );
        }
        let mut tampered = envelope.clone();
        tampered.author = Ed25519PublicKey::from_bytes(other.device_public_key);
        assert!(
            peer.lookup_exact_local_document("test.app", "tab:1", &tampered, now)
                .is_err(),
            "an author changed under the recorded signature"
        );
        // The same write signed by another of the workspace's devices as its
        // own is valid, and is not the evidence this one left.
        let mut resigned = envelope;
        resigned.author = Ed25519PublicKey::from_bytes(other.device_public_key);
        collab_types::sign_op(&mut resigned, &other_key).unwrap();
        assert_eq!(
            peer.lookup_exact_local_document("test.app", "tab:1", &resigned, now)
                .unwrap(),
            None,
            "another valid signer's envelope"
        );
        assert_eq!(
            peer.durable().unwrap(),
            before,
            "no lookup changes the state"
        );
    }

    /// The daemon and its workspace service are two writers of one device under its one key.
    /// Each drafts a write from the record as it sees it, so two drafts can carry one number;
    /// the device's one count, the record's floor in its owner's turn, numbers the later one past
    /// the earlier before it is signed again. Interleaved either way round, every write the
    /// device admits carries a number of its own, strictly increasing, and another device takes
    /// them all with none clashing.
    #[test]
    fn a_devices_two_writers_interleave_under_its_one_count() {
        use automerge::transaction::Transactable as _;
        use collab_types::CommitNamed as _;
        let (mut peer, key, _, _, now) = crate::admission_tests::seeded();
        peer.select_exact_admissions().unwrap();
        let mut receiver = peer.clone();
        let device = key.verifying_key().to_bytes();
        let person = peer
            .certificates()
            .into_iter()
            .find(|cert| cert.device_public_key == device)
            .expect("the workspace certifies the device")
            .user_public_key;
        let draft = |view: &StatePeer, writer: &str, round: i64| -> NeighbourMsg {
            let mut doc =
                automerge::AutoCommit::load(&view.document("test.app", "tab:1").unwrap()).unwrap();
            let heads = doc.get_heads();
            doc.put(automerge::ROOT, writer, round).unwrap();
            doc.commit_edit("fixture", writer, &person);
            view.clone()
                .publication_mut()
                .publish_incremental("test.app", "tab:1", doc.save_after(&heads), &key, now)
                .unwrap()
        };
        let mut admitted = Vec::new();
        for round in 0..6 {
            // Both draft from the record as it stands, before either write is taken.
            let daemon = draft(&peer, "daemon", round);
            let service = draft(&peer, "service", round);
            let (
                NeighbourMsg::AppDocument {
                    envelope: first, ..
                },
                NeighbourMsg::AppDocument {
                    envelope: second, ..
                },
            ) = (&daemon, &service)
            else {
                panic!("document writes")
            };
            assert_eq!(
                first.seq, second.seq,
                "two drafts from one view carry one number"
            );
            let order = if round % 2 == 0 {
                [daemon, service]
            } else {
                [service, daemon]
            };
            for mut message in order {
                let NeighbourMsg::AppDocument { envelope, .. } = &mut message else {
                    panic!("a document write")
                };
                let (mut write, again) = peer
                    .sequenced_local_document("test.app", "tab:1", (**envelope).clone(), now)
                    .unwrap();
                if again {
                    collab_types::sign_op(&mut write, &key).unwrap();
                }
                peer.admit_new_local_document("test.app", "tab:1", write.clone(), now)
                    .unwrap();
                **envelope = write;
                admitted.push(message);
            }
        }
        let numbers: Vec<u64> = admitted
            .iter()
            .map(|message| match message {
                NeighbourMsg::AppDocument { envelope, .. } => envelope.seq,
                _ => panic!("a document write"),
            })
            .collect();
        assert!(
            numbers.windows(2).all(|pair| pair[0] < pair[1]),
            "every write has a number of its own, in the order it was taken: {numbers:?}"
        );
        let origin = crate::TransportOrigin::ServerDelta(key.verifying_key().to_bytes());
        for (at, message) in admitted.into_iter().enumerate() {
            let report = receiver
                .receive_from_report(origin, message, now)
                .expect("another device takes every write of the one count");
            assert!(
                matches!(report.primary(), crate::InputDisposition::Accepted(_)),
                "write {at} is taken: {:?}",
                report.primary()
            );
        }
        assert!(receiver.copied_devices().is_empty());
        let heads = |copy: &StatePeer| {
            let mut heads =
                automerge::AutoCommit::load(&copy.document("test.app", "tab:1").unwrap())
                    .unwrap()
                    .get_heads();
            heads.sort();
            heads
        };
        assert_eq!(heads(&receiver), heads(&peer), "both have every write");
    }
}
