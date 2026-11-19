//! Deciding a write: local admissions and the admission core they share.

use super::{
    AcceptedCheckpoint, ActionKind, AdmissionError, AdmissionReport, ArtifactKind, AutoCommit,
    BTreeMap, Document, DocumentAdmission, DocumentMutation, DroppedInput, Duration,
    Ed25519PublicKey, Held, InboundState, InputDisposition, MemberAccess, MissingPrerequisite,
    NeighbourMsg, OpKind, ReceiveOutcome, Route, SignedOp, StatePeer, SystemTime, backend,
    empty_genesis, empty_genesis_op, err, peer_handle, person_of, sealed_compaction,
    sealed_history, workspace,
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
                return Err("local app publication cannot create a record owner".into());
            }
            if genesis
                && !self.signed_by_owner(&source)
                && !self.may_create_document(
                    &self.membership(source, now)?,
                    &Route {
                        app: app.into(),
                        record: record.into(),
                    },
                    now,
                )
            {
                return Err("the workspace does not let this device create the record".into());
            }
        }
        self.admit_document(
            source,
            NeighbourMsg::AppDocument {
                peer: peer_handle(&source),
                app_id: app.into(),
                cluster_id: self.core.workspace.clone(),
                document_id: record.into(),
                envelope: Box::new(envelope),
                own: self.core.own_write(app, record, &source),
                readers: readers.map(str::to_owned),
            },
            now,
            true,
            false,
            false,
        )
        .map_err(|error| match error {
            AdmissionError::ChangeRefused(refused) => crate::NotAdmitted::ChangeRefused(refused),
            other => crate::NotAdmitted::Refused(other.to_string()),
        })
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
            own,
            readers,
        } = message
        else {
            return Err("not a document message".into());
        };
        if cluster_id != self.core.workspace
            || peer != peer_handle(&source)
            || envelope.author.as_bytes() != &source
        {
            return Err("transport author or document workspace mismatch".into());
        }
        if !retained_replay
            && (own.is_some()
                || self
                    .core
                    .own_write(&app_id, &document_id, &source)
                    .is_some())
        {
            return Err(collab_types::own_resource::ARCHIVE_READ_ONLY.into());
        }
        self.core.learn_own(&app_id, &document_id, own.as_ref());
        self.check_document_key_scope(
            (&app_id, &document_id),
            readers.as_deref(),
            envelope.encryption,
            now,
            retained_replay,
        )?;
        // A piece of a compaction that spans several ops changes nothing on its
        // own: it is gathered, and the compaction is admitted once it is whole.
        let envelope = if collab_types::is_compaction_piece(&envelope)
            && !self
                .publication()
                .holds_sealed(&app_id, &document_id, &envelope)
        {
            match self.gather_compaction_piece((&app_id, &document_id), *envelope)? {
                Some(head) => Box::new(head),
                None => return Ok(AdmissionReport::metadata(ReceiveOutcome::default())),
            }
        } else {
            envelope
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
        let (member, history) = match self
            .authorize_document(
                &source,
                &app_id,
                &document_id,
                &envelope,
                authorization_time,
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
                other @ super::authority::AuthorizeRefusal::Denied(_) => {
                    AdmissionError::Rejected(other.to_string())
                }
            })? {
            // A device its person removed writes nothing more: only what the
            // workspace had taken of it is history, and a write this device
            // makes now is never that.
            super::authority::Authorized::Admit { history: true, .. }
            | super::authority::Authorized::Hold(MissingPrerequisite::RemovalCut)
                if local_app =>
            {
                return Err(super::MembershipError::Revoked.to_string().into());
            }
            super::authority::Authorized::Admit { member, history } => (member, history),
            super::authority::Authorized::Hold(prerequisite) => {
                return self.defer(
                    source,
                    NeighbourMsg::AppDocument {
                        peer,
                        app_id,
                        cluster_id,
                        document_id,
                        envelope,
                        own: own.clone(),
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
        // Whether the change's writer is one of the owner's devices: a device
        // of theirs removed since wrote its history as one.
        let by_owner = self.wrote_as_owner(&source, history);
        let payload = collab_types::validate_whole_document_payload_route(
            &app_id,
            &cluster_id,
            &document_id,
            &envelope,
        )
        .map_err(err)?;
        // A person's view of the workspace's layout is theirs: every member reads it, and only they
        // write it. The workspace's own host commits the documents of its records, so its writes
        // are the workspace's like any other.
        if !by_owner {
            tilo_desktop_model::shared_resource::guard::may_write(
                &app_id,
                &document_id,
                person_of(&member)?,
            )
            .map_err(|refusal| format!("document change forbidden: {refusal}"))?;
        }
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
        if let Some(person) = self.core.own_person(&route.app, &route.record, &source) {
            candidate
                .channel
                .artifact_owners
                .insert(envelope.artifact, person);
        }
        candidate.channel.workspace.clone_from(&self.core.workspace);
        let mut inbound = candidate.channel.clone();
        inbound.grants.clone_from(&member.policy.grants);
        inbound.surfaces.clone_from(&self.surfaces);
        match collab_types::validate_admitted_inbound_op(
            &envelope,
            &member,
            &mut inbound,
            authorization_time,
            if historical_validation {
                Duration::ZERO
            } else {
                Duration::from_secs(300)
            },
        ) {
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
                // a governor following its store, can hand a write over after
                // one of a later number. A sealed write this document does not
                // hold is no replay of what it does, whatever its number, and
                // leaving it out would put the history a write short of what its
                // author's compaction covers, so every compaction of the
                // document would be refused. Only a write the history holds
                // already, or a fold over it already carries, is a replay.
                let replayed = candidate
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
                return Ok(AdmissionReport::new(
                    InputDisposition::RetryLater(MissingPrerequisite::ExactHistoricalEvidence),
                    ReceiveOutcome {
                        duplicate: true,
                        ..Default::default()
                    },
                ));
            }
            Err(error) => return Err(err(error).into()),
        }
        if envelope.kind == OpKind::Compaction
            && !matches!(
                payload,
                collab_types::WholeDocumentPayload::Encrypted { .. }
            )
        {
            return Err("a document compaction is only ever sealed".into());
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
                        own: own.clone(),
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
                            own: own.clone(),
                            readers,
                        },
                        Held {
                            retained_replay,
                            prerequisite: MissingPrerequisite::DocumentCheckpoint,
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
                return Err(
                    "incomplete: authenticated persisted MLS group required; plaintext downgrade forbidden"
                        .into(),
                );
            }
        };
        candidate.channel = inbound;
        if let DocumentMutation::Incremental(bytes) = &mutation {
            let changes = collab_types::parse_changes(bytes).map_err(err)?;
            let shows = self.appearance_author(&source, authorization_time, history);
            collab_types::check_appearances(&changes, shows.as_ref())
                .map_err(|refusal| format!("document change forbidden: {refusal}"))?;
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
                    own: own.clone(),
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
        // checkpoint the workspace governs, with its coverage row fixing the
        // history it retains. Any other writer publishes the record as it
        // holds it, which is its own change and merges like one: a document
        // is merged into what a device holds, never put in its place, so
        // nothing of the record is lost by taking it.
        // One checkpoint governs a route: the one its coverage row names,
        // which is the first of the owner's devices to cover it. Another of
        // their devices publishing the record as it holds it carries no row
        // of its own, and is taken in as that device's own change rather
        // than held to a coverage that is not about it.
        let coverage = self.governed_coverage(&route)?;
        let covered_by = coverage.as_ref().map(|row| row.author);
        let owner_snapshot = matches!(&mutation, DocumentMutation::Snapshot(_))
            && !local_app
            && !genesis
            && by_owner;
        let governed_checkpoint =
            owner_snapshot && covered_by.is_none_or(|author| author == source);
        if governed_checkpoint {
            let DocumentMutation::Snapshot(bytes) = &mutation else {
                return Err("only a whole document is a checkpoint".into());
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
                        own: own.clone(),
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
                        own: own.clone(),
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
                // A strictly newer covered checkpoint by the same author governs
                // this route; skip this one unapplied instead of wedging delivery.
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
                    return Err("retained checkpoint snapshot is incomplete".into());
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
                return Err("checkpoint does not match governed coverage".into());
            }
            if coverage.watermarks.get(&hex::encode(source)).copied() != Some(envelope.seq) {
                return Err("checkpoint author coverage mismatch".into());
            }
            candidate.covered_snapshot = Some(AcceptedCheckpoint::new(bytes.clone()));
            for (author, sequence) in &coverage.watermarks {
                let key: [u8; 32] = hex::decode(author)
                    .map_err(err)?
                    .try_into()
                    .map_err(|_| "coverage key length")?;
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
                    own: own.clone(),
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
                        own: own.clone(),
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
                return Err("local publication has unresolved document dependencies".into());
            }
        };
        if local_app && !candidate.doc.get_missing_deps(&[]).is_empty() {
            return Err("local publication has unresolved document dependencies".into());
        }
        if let Some(before) = &desktop_before {
            self.check_shared_desktop(&candidate.doc, before, &member, authorization_time)?;
        }
        if let Some(workspace) = self.core.workspace_doc.as_deref()
            && let Some(policy) = workspace::media::parse(workspace).map_err(err)?
        {
            workspace::media::validate_bound_chat_change(
                &policy,
                &route.app,
                &route.record,
                &*candidate.doc,
            )
            .map_err(err)?;
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
                person: person_of(member)?,
                members: members.as_ref(),
                allows: &allows,
            },
        )
        .map_err(|refusal| format!("document change forbidden: {refusal}").into())
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
            .transpose()?;
        // The number the compaction takes is its author's too: it stands for
        // that write of theirs now.
        let own = (*envelope.author.as_bytes(), envelope.seq);
        let segments = self.compaction_segments(&route, &envelope)?;
        let gathered = !segments.is_empty();
        Self::retain_sealed(&mut candidate, envelope, segments)?;
        let primary = if self.core.exact_selected {
            // The writes the compaction folded are in its snapshot now, and
            // the number it took with them: their exact evidence becomes one
            // mark per writer, the compaction's own included. This goes first,
            // so the compaction's number is covered by that mark rather than
            // taken as free.
            if let Some(mut folded) = folded {
                folded.push(own);
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
        if gathered {
            self.forget_gathered(&route);
        }
        self.core.documents.insert(route, candidate);
        Ok(AdmissionReport::new(
            primary,
            ReceiveOutcome {
                changed: true,
                metadata_changed: true,
                ..Default::default()
            },
        ))
    }
    fn retain_sealed(
        candidate: &mut Document,
        envelope: SignedOp,
        segments: Vec<SignedOp>,
    ) -> Result<(), AdmissionError> {
        let full = || -> AdmissionError {
            "sealed record history is full; a key holder must compact it (PeerPublication::draft_sealed_compaction)"
                .into()
        };
        if envelope.kind == OpKind::Compaction {
            let history: backend::SealedLog = sealed_compaction::supersede(
                &candidate.sealed.to_vec(),
                &candidate.channel.last_seq_per_author,
                &envelope,
                segments,
            )?
            .into();
            if !history.within_bounds() {
                return Err(full());
            }
            candidate.sealed = history;
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
    /// ([`crate::PeerPublication::draft_sealed_compaction`]). A write in the clear keeps no sealed history.
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
    pub(super) fn restore_sealed(
        &mut self,
        route: &Route,
        ops: Vec<SignedOp>,
    ) -> Result<(), String> {
        sealed_history::restore(&mut self.core, route, ops)
    }
}
