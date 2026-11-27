//! Deciding a write: local admissions and the admission core they share.

use super::{
    AcceptedCheckpoint, ActionKind, AdmissionError, AdmissionReport, ArtifactKind, AutoCommit,
    BTreeMap, Document, DocumentAdmission, DocumentMutation, DroppedInput, Duration,
    Ed25519PublicKey, Held, InboundState, InputDisposition, MemberAccess, MissingPrerequisite,
    NeighbourMsg, OpKind, ReceiveOutcome, Route, SignedOp, StatePeer, SystemTime, backend,
    empty_genesis, empty_genesis_op, peer_handle, person_of, sealed_compaction,
};
use crate::document;

use automerge::transaction::Transactable as _;

impl StatePeer {
    /// Host-local SDK publication after its certified connection was admitted.
    /// No wire message can select this path. Unlike a retained checkpoint, an
    /// SDK snapshot is an authored mutation of an already-owned exact record.
    /// A write that is the whole record, sealed, is the one exception: it
    /// names the document on its own, so a device
    /// founds it with it, as a replica that holds none of the route founds it
    /// from one it is sent.
    pub fn admit_local_app_document(
        &mut self,
        app: &str,
        record: &str,
        envelope: collab_types::SignedOp,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        self.admit_local((app, record), None, envelope, now)
            .map(|report| report.summary)
            .map_err(Into::into)
    }

    pub(super) fn admit_local(
        &mut self,
        (app, record): (&str, &str),
        readers: Option<&str>,
        envelope: collab_types::SignedOp,
        now: SystemTime,
    ) -> Result<AdmissionReport, crate::NotAdmitted> {
        // A record whose stored history is set aside keeps none of what is written to it until a
        // device sends its history again: a write taken now would not survive a restart.
        if self.quarantined_route(app, record) {
            return Err(crate::NotAdmitted::Restoring);
        }
        let source = *envelope.author.as_bytes();
        if self.document(app, record).is_none() {
            // A local publication is an authored change to a record the
            // workspace owns already, or a write that founds one: the whole
            // record, which the member's own permissions decide as a peer's
            // first write is, or the empty genesis only a device the workspace
            // lets create the record starts it with, as a peer's genesis is
            // held until it may. A record this device seals refuses the
            // cleartext below, whichever of the two it is.
            let genesis = empty_genesis_op(app, record, &self.core.workspace, &envelope);
            if !envelope.is_whole_resource() && !genesis {
                return Err(AdmissionError::malformed(
                    "local app publication cannot create a record owner",
                )
                .into());
            }
            if genesis
                && !self.signed_by_administrator(&source)
                && !self.may_create_document(
                    &self.permissions(source, now)?,
                    &Route {
                        app: app.into(),
                        record: record.into(),
                    },
                    now,
                )
            {
                return Err(AdmissionError::forbidden(
                    "the workspace does not let this device create the record",
                )
                .into());
            }
        }
        let message = NeighbourMsg::AppDocument {
            peer: peer_handle(&source),
            app_id: app.into(),
            cluster_id: self.core.workspace.clone(),
            document_id: record.into(),
            envelope: Box::new(envelope),
            readers: readers.map(str::to_owned),
        };
        let report = self
            .admit_document(source, message.clone(), now, true, false, false)
            .map_err(crate::NotAdmitted::from)?;
        // A local write that waits is not held: its author keeps it and writes it again, so the
        // number it was drafted under stays the one the next change takes, and that change is
        // the only one ever applied under it.
        if report.summary.deferred {
            self.forget_held(source, &message);
            self.forget_refused(source, super::written_route(&message).as_ref());
        }
        Ok(report)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the single document admission path; each check builds on the previous"
    )]
    pub(super) fn admit_document(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        local_app: bool,
        retained_replay: bool,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            document_id,
            envelope,
            readers,
        } = message
        else {
            return Err(crate::AdmissionError::malformed("not a document message"));
        };
        if cluster_id != self.core.workspace
            || peer != peer_handle(&source)
            || envelope.author.as_bytes() != &source
        {
            return Err(crate::AdmissionError::forbidden(
                "transport author or document workspace mismatch",
            ));
        }
        // A frame is admitted only as the device whose key signed it. The
        // store's keep checks this too (`retained::Frame::of`), but it runs
        // after the engine has decided the frame, so a write that reached the
        // engine any other way would otherwise decide an op no key signed.
        collab_types::verify_op(&envelope).map_err(|error| {
            crate::AdmissionError::forbidden(format!(
                "the change is not signed by its author: {error}"
            ))
        })?;
        self.check_document_key_scope(
            (&app_id, &document_id),
            readers.as_deref(),
            envelope.encryption,
            now,
            retained_replay,
        )
        .map_err(crate::AdmissionError::malformed)?;
        // A device keeps what its own person reads, never what a sender offers it: a change to a
        // record outside the part of the workspace their role covers is let go here, unkept,
        // whatever its author may write there. The sender's own filter is no offer.
        if !local_app && self.outside_own_part((&app_id, &document_id), now) {
            let input = DroppedInput {
                author: source,
                app: app_id,
                record: document_id,
                sequence: envelope.seq,
                waiting_for: None,
                reason: crate::admission::DropReason::OutsidePart,
            };
            return Ok(AdmissionReport::new(
                InputDisposition::OutsidePart,
                ReceiveOutcome {
                    dropped: vec![input],
                    ..Default::default()
                },
            ));
        }
        // A piece of a compaction that spans several ops changes nothing on its
        // own: it is gathered, and the compaction is admitted once it is whole.
        // The whole one is an op of its own, its signature not yet verified.
        let (envelope, verified) = if collab_types::is_compaction_piece(&envelope)
            && !self
                .publication()
                .holds_sealed(&app_id, &document_id, &envelope)
        {
            match self
                .gather_compaction_piece((&app_id, &document_id), *envelope)
                .map_err(crate::AdmissionError::malformed)?
            {
                Some(head) => (Box::new(head), false),
                None => return Ok(AdmissionReport::metadata(ReceiveOutcome::default())),
            }
        } else {
            (envelope, true)
        };
        if strict_report
            && let Some(admitted) = self.lookup_exact_document_parts(
                source,
                &app_id,
                &cluster_id,
                &document_id,
                &envelope,
                now,
            )?
        {
            return Ok(AdmissionReport::new(
                InputDisposition::Duplicate(admitted),
                ReceiveOutcome {
                    duplicate: true,
                    ..Default::default()
                },
            ));
        }
        let route = Route {
            app: app_id.clone(),
            record: document_id.clone(),
        };
        if !local_app
            && self
                .overtakes_let_go(&route, source, envelope.seq)
                .is_some()
        {
            // Let go too: the workspace's host retains it and replays it after
            // the earlier write, in the order its author wrote them.
            let waiting = MissingPrerequisite::EarlierWrite;
            let input = DroppedInput {
                author: source,
                app: app_id,
                record: document_id,
                sequence: envelope.seq,
                waiting_for: Some(waiting.clone()),
                reason: crate::admission::DropReason::Superseded,
            };
            self.remember_missed(&input, envelope.encryption.is_none());
            return Ok(AdmissionReport::new(
                InputDisposition::RetryLater(waiting),
                ReceiveOutcome {
                    dropped: vec![input],
                    ..Default::default()
                },
            ));
        }
        // A retained frame is history by nature: its author signed it when it
        // was fresh, and a store or relay hands it over later. It is decided as
        // of its own timestamp, never re-signed.
        let historical_validation = retained_replay;
        let authorization_time = if historical_validation {
            envelope.timestamp
        } else {
            now
        };
        let (member, by_owner) = match self
            .authorize_document(
                &source,
                &app_id,
                &document_id,
                &envelope,
                authorization_time,
                retained_replay,
            )
            .map_err(|refusal| match refusal {
                super::authority::AuthorizeRefusal::Rules(refusal, said) => {
                    AdmissionError::ChangeRefused(Box::new(crate::ChangeRefused::of(
                        self.core.workspace.clone(),
                        cluster_id.clone(),
                        &app_id,
                        &document_id,
                        &envelope,
                        &refusal,
                        said,
                    )))
                }
                super::authority::AuthorizeRefusal::Denied(refusal) => refusal,
            })? {
            // A device its person removed writes nothing more: only what the
            // workspace had taken of it is history, and a write this device
            // makes now is never that.
            super::authority::Authorized::Admit { history: true, .. }
            | super::authority::Authorized::Hold(MissingPrerequisite::RemovalCut)
                if local_app =>
            {
                return Err(super::MembershipError::Revoked.into());
            }
            super::authority::Authorized::Admit {
                member, by_owner, ..
            } => (*member, by_owner),
            super::authority::Authorized::Hold(prerequisite) => {
                return self.defer(
                    source,
                    NeighbourMsg::AppDocument {
                        peer,
                        app_id,
                        cluster_id,
                        document_id,
                        envelope,
                        readers,
                    },
                    Held {
                        retained_replay,
                        prerequisite,
                        now,
                    },
                );
            }
        };
        let payload = collab_types::validate_whole_document_payload_route(
            &app_id,
            &cluster_id,
            &document_id,
            &envelope,
        )
        .map_err(crate::AdmissionError::malformed)?;
        let route = Route {
            app: app_id.clone(),
            record: document_id,
        };
        let route_existed = self.core.documents.contains_key(&route);
        let mut candidate = self.core.documents.get(&route).cloned().unwrap_or_else(|| {
            Document {
                doc: AutoCommit::new().into(),
                channel: InboundState::default(),
                encryption_required: false,
                covered: BTreeMap::new(),
                covered_snapshot: None,
                sealed: backend::SealedLog::default(),
                resealed: std::sync::Arc::default(),
                checkpoint_writes: std::sync::Arc::default(),
                quarantine: None,
            }
            .into()
        });
        candidate
            .channel
            .artifacts
            .insert(envelope.artifact, ArtifactKind::Note);
        candidate
            .channel
            .artifact_apps
            .insert(envelope.artifact, app_id);
        candidate
            .channel
            .artifact_resources
            .insert(envelope.artifact, route.record.clone());
        candidate.channel.workspace.clone_from(&self.core.workspace);
        let mut inbound = candidate.channel.clone();
        inbound.grants.clone_from(&member.policy.grants);
        inbound.surfaces.clone_from(&self.surfaces);
        // A write an import step holds: what it made naming no one is the person's.
        inbound.imported = self.governance().imported_invoker(
            envelope.author.as_bytes(),
            &route.app,
            &route.record,
            envelope.seq,
        );
        // The author's grants, and the call the change was made for, are
        // decided above ([`Self::authorize_document`]): this takes the write
        // into the channel's log, checked for its signature (unless verified
        // above), its time and its place there, and never decided a second time.
        let time = if verified {
            collab_types::validator::inbound_time
        } else {
            collab_types::validator::verify_inbound_time
        };
        let skew = if historical_validation {
            Duration::ZERO
        } else {
            Duration::from_secs(300)
        };
        match time(&envelope, authorization_time, skew, historical_validation)
            .and_then(|()| collab_types::validator::validate_log(&envelope, &mut inbound))
        {
            Ok(()) => {}
            Err(collab_types::ValidationError::ReplaySeq { .. })
                if !local_app
                    && matches!(
                        &payload,
                        collab_types::WholeDocumentPayload::Cleartext(DocumentMutation::Snapshot(
                            _
                        ))
                    )
                    && self.superseded_checkpoint(&route, &source, envelope.seq)? =>
            {
                // Signed and authorised, but an older checkpoint than the one
                // governance already covers for this route: never applied.
                return Ok(Self::superseded_checkpoint_report());
            }
            Err(collab_types::ValidationError::ReplaySeq { .. }) => {
                // A sealed history is a log, kept in whatever order its writes
                // were admitted: a replica catching up from retained frames, or
                // a host following its store, can hand a write over after
                // one of a later number. A sealed write this document does not
                // hold is no replay of what it does, whatever its number, and
                // leaving it out would put the history a write short of what its
                // author's compaction covers, so every compaction of the
                // document would be refused. Only a write the history holds
                // already, or a fold over it already carries, is a replay. A
                // fold this peer took leaves its writers' marks behind, and the
                // fold leading the history now may be a later one that names
                // none of them: a write a mark covers is decided, and taking it
                // again would keep a write the index holds no evidence of.
                let replayed = self
                    .core
                    .exact_admissions
                    .covers(&crate::admission::AdmissionKey {
                        workspace: self.core.workspace.clone(),
                        app: route.app.clone(),
                        record: route.record.clone(),
                        author: *envelope.author.as_bytes(),
                        sequence: envelope.seq,
                    })
                    || candidate
                        .sealed
                        .iter()
                        .any(|held| held.author == envelope.author && held.seq == envelope.seq)
                    || candidate
                        .sealed
                        .first()
                        .filter(|first| !collab_types::is_compaction_segment(first))
                        .and_then(|first| {
                            tilo_failure::ignored!(
                                "a first write that does not parse as a fold carries no writes",
                                collab_types::SealedCompaction::parse(first).ok()
                            )
                        })
                        .is_some_and(|fold| fold.covers(&envelope));
                if candidate.encryption_required
                    && matches!(
                        &payload,
                        collab_types::WholeDocumentPayload::Encrypted { .. }
                    )
                    && !replayed
                {
                    return self.admit_sealed(route, candidate, inbound, *envelope);
                }
                // A write the record's checkpoint stands in for, which this device took without
                // it: kept beside the checkpoint, which is read once all of them are here.
                if candidate.encryption_required
                    && let Some(kept) = self.keep_checkpoint_write(&route, &envelope)
                {
                    return kept;
                }
                return Ok(AdmissionReport::new(
                    InputDisposition::RetryLater(MissingPrerequisite::ExactHistoricalEvidence),
                    ReceiveOutcome {
                        duplicate: true,
                        ..Default::default()
                    },
                ));
            }
            Err(error) => return Err(crate::AdmissionError::malformed(error)),
        }
        if envelope.kind == OpKind::Compaction
            && !matches!(
                payload,
                collab_types::WholeDocumentPayload::Encrypted { .. }
            )
        {
            return Err(crate::AdmissionError::malformed(
                "a document compaction is only ever sealed",
            ));
        }
        let mutation = match payload {
            // The write that seals the record waits behind the record's
            // cleartext writes held here: they came before it in the record's
            // order, so they are its base, decided before it seals.
            collab_types::WholeDocumentPayload::Encrypted { .. }
                if !candidate.encryption_required && self.holds_cleartext_of(&route) =>
            {
                return self.defer(
                    source,
                    NeighbourMsg::AppDocument {
                        peer,
                        app_id: route.app,
                        cluster_id,
                        document_id: route.record,
                        envelope,
                        readers,
                    },
                    Held {
                        retained_replay,
                        prerequisite: MissingPrerequisite::CausalDependencies,
                        now,
                    },
                );
            }
            // A record's cleartext writes before it sealed are the base its
            // sealed writes are changes over, and arrive before them in the
            // order its host keeps it: a device that joins or returns takes
            // that base in first. Whether a write is history is where it
            // stands in the record, never how it travelled.
            collab_types::WholeDocumentPayload::Cleartext(mutation)
                if !candidate.encryption_required =>
            {
                // The record's own document holds it (see `covered`), so
                // it covers this writer up to it.
                let covered = candidate.covered.entry(hex::encode(source)).or_default();
                *covered = (*covered).max(envelope.seq);
                mutation
            }
            collab_types::WholeDocumentPayload::Encrypted { .. } if !self.core.exact_selected => {
                // A replica without exact admission evidence keeps no sealed
                // history: it only records that the record is encrypted, so a
                // later plaintext write cannot downgrade it.
                candidate.encryption_required = true;
                self.core.documents.insert(route, candidate);
                return Ok(AdmissionReport::new(
                    InputDisposition::RetryLater(MissingPrerequisite::PersistedMls),
                    ReceiveOutcome {
                        metadata_changed: true,
                        incomplete: self.incomplete_reason(),
                        ..Default::default()
                    },
                ));
            }
            collab_types::WholeDocumentPayload::Encrypted { .. } => {
                // A sealed change needs the record it changes; a write that
                // is the whole record — a compaction, or a checkpoint such as
                // the latest sealed version the workspace keeps —
                // founds the route for a replica that holds none of it, as a
                // device catching up from a store that holds only ciphertext
                // does.
                if !route_existed && !envelope.is_whole_resource() {
                    return self.defer(
                        source,
                        NeighbourMsg::AppDocument {
                            peer,
                            app_id: route.app,
                            cluster_id,
                            document_id: route.record,
                            envelope,
                            readers,
                        },
                        Held {
                            retained_replay,
                            prerequisite: MissingPrerequisite::DocumentCheckpoint,
                            now,
                        },
                    );
                }
                // A checkpoint carries each author's last write it holds:
                // one that does not is no checkpoint.
                if collab_types::is_sealed_checkpoint(&envelope) {
                    collab_types::SealedCheckpoint::parse(&envelope)
                        .map_err(AdmissionError::malformed)?;
                }
                // An author's sealed writes are numbered one after another,
                // from where a compaction or a checkpoint left each author's
                // number: a write past the next one this replica expects
                // builds on writes it has not seen, and waits for them as
                // history its sender is asked for. A whole record stands on
                // nothing before it.
                let seen = candidate
                    .channel
                    .last_seq_per_author
                    .get(&envelope.author)
                    .copied()
                    .unwrap_or_default();
                let expected = if envelope.seq > seen.saturating_add(1) {
                    candidate
                        .sealed
                        .iter()
                        .filter_map(|held| {
                            collab_types::SealedCheckpoint::floor_of(held, &envelope.author).ok()
                        })
                        .fold(seen, u64::max)
                } else {
                    seen
                }
                .saturating_add(1);
                if !envelope.is_whole_resource() && envelope.seq > expected {
                    return self.defer(
                        source,
                        NeighbourMsg::AppDocument {
                            peer,
                            app_id: route.app,
                            cluster_id,
                            document_id: route.record,
                            envelope,
                            readers,
                        },
                        Held {
                            retained_replay,
                            prerequisite: MissingPrerequisite::CausalDependencies,
                            now,
                        },
                    );
                }
                return self.admit_sealed(route, candidate, inbound, *envelope);
            }
            collab_types::WholeDocumentPayload::Cleartext(_) => {
                // A plaintext write to a record after it sealed is a
                // downgrade, and refused, however its host sent it: live, or
                // replayed from what its store kept.
                return Err(crate::AdmissionError::forbidden(
                    "incomplete: authenticated persisted MLS group required; plaintext downgrade forbidden",
                ));
            }
        };
        candidate.channel = inbound;
        if let DocumentMutation::Incremental(bytes) = &mutation {
            let changes =
                collab_admission::parse_changes(bytes).map_err(crate::AdmissionError::malformed)?;
            collab_types::check_appearances(&changes, None)
                .map_err(|refusal| format!("document change forbidden: {refusal}"))
                .map_err(crate::AdmissionError::malformed)?;
        }
        // A record's genesis is not a checkpoint: it is started empty, so
        // there is no retained history for the workspace to acknowledge.
        // Whoever the workspace lets start the record may start it, and a
        // device that already holds the record takes a second start of it as
        // the nothing it is: two devices that start the same record each
        // publish their own genesis, and they merge into the one empty
        // record.
        let empty = matches!(&mutation, DocumentMutation::Snapshot(bytes) if empty_genesis(bytes));
        let genesis = empty && !route_existed;
        if genesis && !by_owner && !self.may_create_document(&member, &route, authorization_time) {
            return self.defer(
                source,
                NeighbourMsg::AppDocument {
                    peer,
                    app_id: route.app,
                    cluster_id,
                    document_id: route.record,
                    envelope,
                    readers,
                },
                Held {
                    retained_replay,
                    prerequisite: MissingPrerequisite::DocumentCheckpoint,
                    now,
                },
            );
        }
        // A snapshot one of the workspace owner's devices publishes is the
        // checkpoint the workspace covers, with its coverage row fixing the
        // history it retains. Any other writer publishes the record as it
        // holds it, which is its own change and merges like one: a document
        // is merged into what a device holds, never put in its place, so
        // nothing of the record is lost by taking it.
        // One checkpoint covers a route: the one its coverage row names,
        // which is the first of the owner's devices to cover it. Another of
        // their devices publishing the record as it holds it carries no row
        // of its own, and is taken in as that device's own change rather
        // than held to a coverage that is not about it.
        let coverage = self
            .checkpoint_coverage(&route)
            .map_err(crate::AdmissionError::malformed)?;
        let covered_by = coverage.as_ref().map(|row| row.author);
        let owner_snapshot = matches!(&mutation, DocumentMutation::Snapshot(_))
            && !local_app
            && !genesis
            && by_owner;
        let covering_checkpoint =
            owner_snapshot && covered_by.is_none_or(|author| author == source);
        if covering_checkpoint {
            let DocumentMutation::Snapshot(bytes) = &mutation else {
                return Err(crate::AdmissionError::malformed(
                    "only a whole document is a checkpoint",
                ));
            };
            let Some(coverage) = coverage.as_ref() else {
                return self.defer(
                    source,
                    NeighbourMsg::AppDocument {
                        peer,
                        app_id: route.app,
                        cluster_id,
                        document_id: route.record,
                        envelope,
                        readers,
                    },
                    Held {
                        retained_replay,
                        prerequisite: MissingPrerequisite::CheckpointCoverage,
                        now,
                    },
                );
            };
            if coverage.sequence < envelope.seq {
                return self.defer(
                    source,
                    NeighbourMsg::AppDocument {
                        peer,
                        app_id: route.app,
                        cluster_id,
                        document_id: route.record,
                        envelope,
                        readers,
                    },
                    Held {
                        retained_replay,
                        prerequisite: MissingPrerequisite::CheckpointCoverage,
                        now,
                    },
                );
            }
            if coverage.sequence > envelope.seq
                && self.superseded_checkpoint(&route, &source, envelope.seq)?
            {
                // A strictly newer covered checkpoint by the same author is
                // this route's; skip this one unapplied instead of wedging delivery.
                return Ok(Self::superseded_checkpoint_report());
            }
            let mut snapshot = AutoCommit::new();
            match tilo_automerge_isolate::apply(
                &mut snapshot,
                bytes,
                tilo_automerge_isolate::Dependencies::RequireComplete,
            )
            .map_err(AdmissionError::from)?
            {
                tilo_automerge_isolate::Applied::Changed(_) => {}
                tilo_automerge_isolate::Applied::MissingDependencies => {
                    return Err(crate::AdmissionError::malformed(
                        "retained checkpoint snapshot is incomplete",
                    ));
                }
            }
            let heads = snapshot
                .base_heads()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if coverage.workspace != self.core.workspace
                || coverage.app != route.app
                || coverage.record != route.record
                || coverage.author != source
                || coverage.sequence != envelope.seq
                || coverage.payload_hash != *blake3::hash(&envelope.payload).as_bytes()
                || coverage.heads != heads
            {
                return Err(crate::AdmissionError::malformed(
                    "checkpoint does not match its coverage",
                ));
            }
            if coverage.watermarks.get(&hex::encode(source)).copied() != Some(envelope.seq) {
                return Err(crate::AdmissionError::forbidden(
                    "checkpoint author coverage mismatch",
                ));
            }
            candidate.covered_snapshot = Some(AcceptedCheckpoint::new(bytes.clone()));
            for (author, sequence) in &coverage.watermarks {
                let key: [u8; 32] = hex::decode(author)
                    .map_err(crate::AdmissionError::malformed)?
                    .try_into()
                    .map_err(|_| "coverage key length")
                    .map_err(crate::AdmissionError::malformed)?;
                let covered = candidate.covered.entry(author.clone()).or_default();
                *covered = (*covered).max(*sequence);
                let floor = candidate
                    .channel
                    .last_seq_per_author
                    .entry(Ed25519PublicKey::from_bytes(key))
                    .or_default();
                *floor = (*floor).max(*sequence);
            }
        } else if !route_existed && !genesis && !owner_snapshot {
            return self.defer(
                source,
                NeighbourMsg::AppDocument {
                    peer,
                    app_id: route.app,
                    cluster_id,
                    document_id: route.record,
                    envelope,
                    readers,
                },
                Held {
                    retained_replay,
                    prerequisite: MissingPrerequisite::DocumentCheckpoint,
                    now,
                },
            );
        }
        // A member's write of the shared desktop is decided by what each of
        // its changes did ([`Self::check_shared_desktop`]).
        let desktop_before = (!by_owner
            && tilo_desktop_model::shared_resource::guard::is_shared_desktop(
                &route.app,
                &route.record,
            ))
        .then(|| candidate.doc.base_heads());
        let changed = match document::merge_committed_with_status(&mut candidate.doc, &mutation)
            .map_err(AdmissionError::from)?
        {
            document::MergeOutcome::Changed(changed) => changed,
            document::MergeOutcome::MissingDependencies if !local_app => {
                return self.defer(
                    source,
                    NeighbourMsg::AppDocument {
                        peer,
                        app_id: route.app,
                        cluster_id,
                        document_id: route.record,
                        envelope,
                        readers,
                    },
                    Held {
                        retained_replay,
                        prerequisite: MissingPrerequisite::CausalDependencies,
                        now,
                    },
                );
            }
            document::MergeOutcome::MissingDependencies => {
                return Err(crate::AdmissionError::rejected(
                    neighbour_wire::RefusalReason::MissingDependency,
                    "local publication has unresolved document dependencies",
                ));
            }
        };
        if local_app && !candidate.doc.get_missing_deps(&[]).is_empty() {
            return Err(crate::AdmissionError::rejected(
                neighbour_wire::RefusalReason::MissingDependency,
                "local publication has unresolved document dependencies",
            ));
        }
        if let Some(before) = &desktop_before {
            self.check_shared_desktop(&candidate.doc, before, &member, authorization_time)?;
        }
        let primary = if self.core.exact_selected {
            let admitted = DocumentAdmission::from_op(
                &self.core.workspace,
                &route.app,
                &route.record,
                &envelope,
            )?;
            self.core.exact_admissions.insert(&admitted)?;
            InputDisposition::Accepted(admitted)
        } else {
            InputDisposition::MetadataOnly
        };
        self.core.documents.insert(route, candidate);
        Ok(AdmissionReport::new(
            primary,
            ReceiveOutcome {
                changed,
                metadata_changed: true,
                ..Default::default()
            },
        ))
    }
    /// A member's write of the workspace's shared desktop closes, moves or
    /// renames only what its writer made, or what the workspace lets them
    /// moderate ([`tilo_desktop_model::shared_resource::guard`]): decided on
    /// the changes `doc` holds beyond `before`, against the policy this peer
    /// holds.
    fn check_shared_desktop(
        &self,
        doc: &AutoCommit,
        before: &[automerge::ChangeHash],
        member: &MemberAccess,
        now: SystemTime,
    ) -> Result<(), AdmissionError> {
        use tilo_desktop_model::ownership::act_node;
        let members = self
            .core
            .workspace_doc
            .as_deref()
            .and_then(crate::members::people);
        let workspace = &self.core.workspace;
        let allows = |act: &tilo_desktop_model::ownership::Touched| {
            member.allows(&act_node(workspace, act), ActionKind::Undoable, now)
        };
        tilo_desktop_model::shared_resource::guard::judge(
            doc,
            before,
            &tilo_desktop_model::shared_resource::guard::Writer {
                person: person_of(member).map_err(crate::AdmissionError::forbidden)?,
                members: members.as_ref(),
                allows: &allows,
            },
        )
        .map_err(|refusal| {
            crate::AdmissionError::forbidden(format!("document change forbidden: {refusal}"))
        })
    }

    /// Keep a sealed write this replica holds no key for: its author, role,
    /// route, signature and sequence were checked above (`validated` is the
    /// record's channel with its sequence), and the ciphertext itself
    /// becomes the record's durable history, never opened here. A verified
    /// compaction replaces exactly the writes it covers.
    fn admit_sealed(
        &mut self,
        route: Route,
        mut candidate: backend::StoredDocument,
        validated: InboundState,
        envelope: SignedOp,
    ) -> Result<AdmissionReport, AdmissionError> {
        let admitted =
            DocumentAdmission::from_op(&self.core.workspace, &route.app, &route.record, &envelope)?;
        let folded = (envelope.kind == OpKind::Compaction)
            .then(|| sealed_compaction::covered(&envelope))
            .transpose()
            .map_err(crate::AdmissionError::malformed)?;
        // The number the compaction takes is its author's too: it stands for
        // that write of theirs now.
        let own = (*envelope.author.as_bytes(), envelope.seq);
        let segments = self
            .compaction_segments(&route, &envelope)
            .map_err(crate::AdmissionError::malformed)?;
        let gathered = !segments.is_empty();
        // The writes the history's checkpoint stands in for go only once every device with the
        // record's keys checked it; a device with the record's keys founding the record from
        // this compaction, and holding none of what it stands in for, asks for them unless every
        // device with the record's keys checked it.
        let checked = folded.is_some()
            && candidate
                .sealed
                .first()
                .filter(|first| super::checkpoints::is_checkpoint(first))
                .is_some_and(|first| {
                    self.checkpoint_checked_by_all((&route.app, &route.record), first)
                });
        let founding = folded.is_some()
            && candidate.sealed.is_empty()
            && !self.checkpoint_settled((&route.app, &route.record), &envelope);
        Self::retain_sealed(
            &mut candidate,
            (&self.core.workspace, &route),
            envelope,
            segments,
            checked,
        )?;
        let primary = if self.core.exact_selected {
            // The writes the compaction folded are in its snapshot now, and
            // the number it took with them: their exact evidence becomes one
            // mark per writer, the compaction's own included. This goes first,
            // so the compaction's number is covered by that mark rather than
            // taken as free.
            if let Some(folded) = &folded {
                let folded = folded.iter().copied().chain([own]).collect::<Vec<_>>();
                self.core
                    .exact_admissions
                    .cover((&self.core.workspace, &route.app, &route.record), folded)?;
            }
            self.core.exact_admissions.insert(&admitted)?;
            InputDisposition::Accepted(admitted)
        } else {
            InputDisposition::MetadataOnly
        };
        candidate.channel = validated;
        candidate.encryption_required = true;
        super::quarantine::leave_once_whole(&mut candidate);
        if gathered {
            self.forget_gathered(&route);
        }
        self.core.documents.insert(route, candidate);
        // A compaction folds the writes it covers for good: each writer's next write is numbered
        // past the fold, as it is at one a replica takes from its holder
        // ([`PeerPublicationMut::take_sealed_compaction`]). A replica that founded this route from
        // the fold and kept none of those writes as themselves otherwise numbers its own next
        // write under one the fold decided, which its owner could only refuse for good.
        if folded.is_some() {
            self.raise_floors_to_marks();
        }
        Ok(AdmissionReport::new(
            primary,
            ReceiveOutcome {
                changed: true,
                metadata_changed: true,
                lacks_history: founding,
                ..Default::default()
            },
        ))
    }

    /// Keep `envelope`, a sealed write the checkpoint leading the record's history stands in for,
    /// beside it: a device that took the checkpoint without the writes it stands in for asked for
    /// them ([`super::checkpoints`]), and reads the checkpoint once they are here. Its author,
    /// route and signature were checked as any write's; it is no history to replay, and changes
    /// no sequence. `None` when it is no such write, or one kept already.
    fn keep_checkpoint_write(
        &mut self,
        route: &Route,
        envelope: &SignedOp,
    ) -> Option<Result<AdmissionReport, AdmissionError>> {
        // A checkpoint itself is never taken this way: one kept beside the history is either the
        // one every device with the record's keys checked, which leads it, or one read through its
        // own writes.
        if !self.lacks_checkpoint_write(&route.app, &route.record, envelope) {
            return None;
        }
        let document = self.core.documents.get(route)?;
        let admitted = match DocumentAdmission::from_op(
            &self.core.workspace,
            &route.app,
            &route.record,
            envelope,
        ) {
            Ok(admitted) => admitted,
            Err(error) => return Some(Err(error)),
        };
        let mut document = document.clone();
        document.checkpoint_writes = document
            .checkpoint_writes
            .iter()
            .cloned()
            .chain([envelope.clone()])
            .collect();
        self.core.documents.insert(route.clone(), document);
        Some(Ok(AdmissionReport::new(
            InputDisposition::Accepted(admitted),
            ReceiveOutcome {
                changed: true,
                metadata_changed: true,
                ..Default::default()
            },
        )))
    }

    fn retain_sealed(
        candidate: &mut Document,
        (workspace, route): (&str, &Route),
        envelope: SignedOp,
        segments: Vec<SignedOp>,
        checked: bool,
    ) -> Result<(), AdmissionError> {
        let full = || AdmissionError::HistoryFull;
        if envelope.kind == OpKind::Compaction {
            let history: backend::SealedLog = sealed_compaction::supersede(
                &candidate.sealed.to_vec(),
                &candidate.channel.last_seq_per_author,
                &envelope,
                segments,
            )
            .map_err(crate::AdmissionError::malformed)?
            .into();
            if !history.within_bounds() {
                return Err(full());
            }
            candidate.fold_sealed(
                history,
                |op| {
                    collab_types::sealed_before_reader_sets(
                        &route.app,
                        workspace,
                        &route.record,
                        op,
                    )
                },
                checked,
            );
        } else {
            if !Self::has_room(&candidate.sealed, &envelope) {
                return Err(full());
            }
            candidate.sealed.push(envelope);
        }
        Ok(())
    }

    /// Whether `sealed`, a record's sealed history, has room for `envelope`, a write after it.
    fn has_room(sealed: &backend::SealedLog, envelope: &SignedOp) -> bool {
        sealed.takes(envelope.payload.len())
    }

    /// Whether the sealed history of `record` of `app` has room for `envelope`, a sealed write of
    /// this device: a write past it is refused until a key holder compacts the history
    /// ([`crate::PeerPublication::draft_sealed_compaction`]). A write in the clear keeps no sealed
    /// history.
    #[must_use]
    pub fn sealed_history_takes(&self, app: &str, record: &str, envelope: &SignedOp) -> bool {
        if envelope.encryption.is_none() || envelope.kind == OpKind::Compaction {
            return true;
        }
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        self.core
            .documents
            .get(&route)
            .is_none_or(|document| Self::has_room(&document.sealed, envelope))
    }
    /// Reinstall `route`'s stored sealed history, checking every write as it
    /// was admitted: sealed for this exact route, signed by its author, with
    /// exact admission evidence and within the record's watermark and bounds.
    /// A compaction is only ever first and covers none of the later writes.
    #[cfg(test)]
    pub(super) fn restore_sealed(
        &mut self,
        route: &Route,
        ops: Vec<SignedOp>,
    ) -> Result<(), String> {
        super::sealed_history::restore(&mut self.core, route, ops).map_err(|refused| refused.reason)
    }
}
