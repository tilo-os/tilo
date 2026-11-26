//! Taking in what other devices send: workspace changes, record writes, hellos.

use super::{
    AdmissionError, AdmissionReport, DeviceCert, DocumentAdmission, Held, InputDisposition,
    MembershipError, MissingPrerequisite, NeighbourMsg, ReceiveOutcome, Route, SignedOp, StatePeer,
    SystemTime, TransportOrigin, VerifiedCertificate, backend, covered_documents, err, now_ns,
    peer_handle, workspace, written_route,
};

use automerge::transaction::Transactable as _;

impl StatePeer {
    /// Decide received changes by their claims against the app surfaces
    /// `surfaces` finds.
    pub fn set_surfaces(&mut self, surfaces: collab_types::Surfaces) {
        self.surfaces = surfaces;
    }

    /// Keep the exact admission index from now on; the next durable state
    /// holds it.
    pub fn select_exact_admissions(&mut self) -> Result<(), AdmissionError> {
        self.core.exact_admissions.encode()?;
        self.core.exact_selected = true;
        Ok(())
    }
    pub fn exact_admissions_selected(&self) -> bool {
        self.core.exact_selected
    }

    /// Exact observations from the real admission path, not a durability receipt.
    /// A record write is decided against the exact admission index: one
    /// already recorded is a duplicate, other bytes at its sequence conflict.
    pub fn receive_from_report(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<AdmissionReport, AdmissionError> {
        self.admit_from(origin, message, now, true)
    }

    /// Admit a message from `origin`, queuing it while prerequisites are missing.
    /// A record write at or below its writer's floor is a duplicate.
    /// An [`AdmissionError::Device`] is this device failing to decide it, never
    /// the message being refused.
    pub fn receive_from(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, AdmissionError> {
        self.admit_from(origin, message, now, false)
            .map(|report| report.summary)
    }

    /// Admit a message from `origin` on a copy of this state, and keep the
    /// copy once the message is decided. `exact` decides record writes
    /// against the exact admission index rather than the writers' floors.
    fn admit_from(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
        exact: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        // A key found on two machines writes nothing here until one of them is re-added; a
        // change it signed under a number another change of its took here is how it is found.
        let key = super::copied::changed_by(&message);
        if let Some(refused) = key.and_then(|key| self.refused_as_copied(key)) {
            return Err(refused);
        }
        match self.admit_from_one_writer(origin, message, now, exact) {
            Err(AdmissionError::ConflictingSequence) if let Some(key) = key => {
                Err(self.found_copied(key, now))
            }
            admitted => admitted,
        }
    }

    /// [`Self::admit_from`] for a writer this copy takes changes of.
    fn admit_from_one_writer(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
        exact: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        let document = matches!(message, NeighbourMsg::AppDocument { .. });
        if exact && document && !self.core.exact_selected {
            return Err(AdmissionError::WriterSelectionRequired);
        }
        if matches!(origin, TransportOrigin::RetainedNativePublication { .. }) {
            return Err(AdmissionError::malformed(
                "a retained native publication is no longer a carrier",
            ));
        }
        if exact
            && document
            && let Some(report) = self.lookup_exact_document_report(origin, &message, now)?
        {
            return Ok(report);
        }
        let (source, retained_replay) = Self::resolve_origin(origin, &message, now)?;
        let written = written_route(&message);
        let mut candidate = self.clone();
        let received = candidate.receive_with_context(source, message, now, retained_replay, exact);
        let mut report = received.inspect_err(|error| {
            // A device that could not decide the frame has not refused it:
            // what waited for the writer stays waiting for it.
            if !error.is_device_failure() {
                self.forget_refused(source, written.as_ref());
            }
        })?;
        let landed = report.summary.changed || report.summary.metadata_changed;
        report.summary.replay_wanted |= candidate.missed_landed(landed);
        *self = candidate;
        Ok(report)
    }

    /// Authenticate an already recorded document without copying mutable state.
    /// The report observes the existing admission, never a new commit barrier.
    pub fn lookup_exact_document_report(
        &self,
        origin: TransportOrigin,
        message: &NeighbourMsg,
        now: SystemTime,
    ) -> Result<Option<AdmissionReport>, AdmissionError> {
        if !self.core.exact_selected {
            return Err(AdmissionError::WriterSelectionRequired);
        }
        let (source, _) = Self::resolve_origin(origin, message, now)?;
        if self.core.workspace_doc.is_none() || !self.core.certificates.contains_key(&source) {
            return Ok(None);
        }
        // A write whose writer this peer's governance does not list yet is no recorded
        // duplicate: the real admission holds it.
        if matches!(
            backend::member::role(&self.core, &source, now),
            Err(MembershipError::Absent)
        ) {
            return Ok(None);
        }
        self.lookup_exact_document(origin, message, now)
            .map(|exact| {
                exact.map(|admitted| {
                    AdmissionReport::new(
                        InputDisposition::Duplicate(admitted),
                        ReceiveOutcome {
                            duplicate: true,
                            ..Default::default()
                        },
                    )
                })
            })
    }

    /// A read-only exact retry lookup. Current authority and signature/route still
    /// apply; only a recorded envelope bypasses new-write timestamp freshness.
    pub fn lookup_exact_document(
        &self,
        origin: TransportOrigin,
        message: &NeighbourMsg,
        now: SystemTime,
    ) -> Result<Option<DocumentAdmission>, AdmissionError> {
        let (source, _) = Self::resolve_origin(origin, message, now)?;
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            document_id,
            envelope,
            ..
        } = message
        else {
            return Err(crate::AdmissionError::malformed(
                "exact document lookup requires AppDocument",
            ));
        };
        if *peer != peer_handle(&source) {
            return Err(crate::AdmissionError::forbidden(
                "transport author mismatch",
            ));
        }
        self.lookup_exact_document_parts(source, app_id, cluster_id, document_id, envelope, now)
    }
    pub(super) fn lookup_exact_document_parts(
        &self,
        source: [u8; 32],
        app: &str,
        workspace: &str,
        document: &str,
        op: &SignedOp,
        now: SystemTime,
    ) -> Result<Option<DocumentAdmission>, AdmissionError> {
        if workspace != self.core.workspace || op.author.as_bytes() != &source {
            return Err(crate::AdmissionError::forbidden(
                "transport author or document workspace mismatch",
            ));
        }
        // A record whose stored history was set aside holds none of what its evidence names: what
        // a device sends of it again is decided by the real admission, never taken as recorded.
        if self.quarantined_route(app, document) {
            return Ok(None);
        }
        if matches!(
            self.authorize_document(&source, app, document, op, now, false)
                .map_err(|refusal| match refusal {
                    super::authority::AuthorizeRefusal::Rules(refusal, said) => {
                        AdmissionError::ChangeRefused(Box::new(crate::ChangeRefused::of(
                            self.core.workspace.clone(),
                            workspace.to_owned(),
                            app,
                            document,
                            op,
                            &refusal,
                            said,
                        )))
                    }
                    super::authority::AuthorizeRefusal::Denied(refusal) => refusal,
                })?,
            super::authority::Authorized::Hold(_)
        ) {
            // Held for what it waits for: decided by the real admission, never a recorded one.
            return Ok(None);
        }
        collab_types::validate_whole_document_payload_route(app, workspace, document, op)
            .map_err(crate::AdmissionError::malformed)?;
        collab_types::verify_op(op).map_err(crate::AdmissionError::malformed)?;
        let admission = DocumentAdmission::from_op(workspace, app, document, op)?;
        let compaction = op.kind == collab_types::OpKind::Compaction;
        match self.core.exact_admissions.get(&admission.key) {
            Some(id) if id == &admission.operation_id => Ok(Some(admission)),
            // A compaction takes the number of a write of its author it folds,
            // so that number being held is the history it restates, not a
            // conflict: taking it in place of exactly those writes decides it.
            Some(_) if compaction => Ok(None),
            Some(_) => Err(AdmissionError::ConflictingSequence),
            None if self.core.exact_admissions.covers(&admission.key) => {
                // A mark covers it: decided, and ignored like a retry — unless
                // it is a fold that still has a write of the history to fold.
                // A later fold may take the same number, and a segment of one
                // is never evidence itself: it is gathered into its set.
                if collab_types::is_compaction_segment(op)
                    || (compaction && self.folds_held(app, document, op))
                    || self.lacks_checkpoint_write(app, document, op)
                {
                    Ok(None)
                } else {
                    Ok(Some(admission))
                }
            }
            None => Ok(None),
        }
    }

    /// Whether `op` is a sealed write the checkpoint leading the record's history stands in for
    /// that this device keeps no copy of: one taken in now is kept beside the checkpoint, which a
    /// device with the record's keys reads once all of them are here ([`super::checkpoints`]). A
    /// write the checkpoint leading what is kept beside it already stands in for went for good.
    pub(super) fn lacks_checkpoint_write(&self, app: &str, record: &str, op: &SignedOp) -> bool {
        if op.kind != collab_types::OpKind::Change || op.encryption.is_none() {
            return false;
        }
        let Some(document) = self.core.documents.get(&Route {
            app: app.into(),
            record: record.into(),
        }) else {
            return false;
        };
        let covers = |checkpoint: Option<&SignedOp>| {
            checkpoint
                .filter(|first| super::checkpoints::is_checkpoint(first))
                .and_then(|first| {
                    tilo_failure::ignored!(
                        "a first write that does not parse as a checkpoint stands in for nothing",
                        collab_types::SealedCompaction::parse(first).ok()
                    )
                })
                .is_some_and(|wrapper| wrapper.covers(op))
        };
        covers(document.sealed.first())
            && !covers(document.checkpoint_writes.first())
            && !document
                .checkpoint_writes
                .iter()
                .chain(document.sealed.iter())
                // A write of a sequence kept already is that write again, or a second write of
                // its sequence: either way the checkpoint lacks nothing of it. A checkpoint takes
                // the number of a write of its author it folds, and is never that write.
                .any(|kept| {
                    kept.kind == collab_types::OpKind::Change
                        && kept.author == op.author
                        && kept.seq == op.seq
                })
    }

    /// Whether `compaction` folds a write this replica still holds as history:
    /// a fold of nothing held restates what an earlier fold already did.
    fn folds_held(&self, app: &str, record: &str, compaction: &SignedOp) -> bool {
        let Ok(wrapper) = collab_types::SealedCompaction::parse(compaction) else {
            return false;
        };
        self.core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .is_some_and(|document| {
                document
                    .sealed
                    .iter()
                    .filter(|write| write.kind != collab_types::OpKind::Compaction)
                    .any(|write| wrapper.covers(write))
            })
    }

    /// Whether this state, staged from `before`, has `before`'s owner, or
    /// the one its governance names after a transfer it replays: the one way
    /// a workspace's owner changes.
    pub fn keeps_owner_of(&self, before: &Self) -> bool {
        self.core.owner == before.core.owner
            || self
                .core
                .workspace_doc
                .as_ref()
                .and_then(|doc| listed_owner(doc, &self.core.workspace))
                == Some(self.core.owner)
    }

    /// Exact new candidate admissions for local multi-record owner commits.
    /// Also refuses replacement that discards prior exact evidence.
    pub fn admissions_since(
        &self,
        before: &StatePeer,
    ) -> Result<Vec<DocumentAdmission>, AdmissionError> {
        if self.core.workspace != before.core.workspace
            || !self.keeps_owner_of(before)
            || (before.core.exact_selected && !self.core.exact_selected)
        {
            return Err(crate::AdmissionError::rejected(
                neighbour_wire::RefusalReason::Conflict,
                "admission candidate replaces namespace or downgrades native writer",
            ));
        }
        let index = &self.core.exact_admissions;
        let prior = &before.core.exact_admissions;
        // A candidate staged from `before` shares its settled entries and
        // marks until it settles or covers some itself: then both hold
        // the same beyond their recent entries, and only those differ.
        // Walking the whole index made each governance change cost every
        // write the workspace ever kept.
        if index.shares_settled(prior) {
            for (key, id) in prior.recent() {
                if index.get(key) != Some(id) {
                    return Err(crate::AdmissionError::rejected(
                        neighbour_wire::RefusalReason::Conflict,
                        "admission candidate discards exact evidence",
                    ));
                }
            }
            return Ok(index
                .recent()
                .filter(|(key, _)| prior.get(key).is_none())
                .map(|(key, id)| DocumentAdmission {
                    key: key.clone(),
                    operation_id: *id,
                })
                .collect());
        }
        // Evidence may only be kept exactly or folded under a mark that a
        // compaction raised; marks never fall.
        for (key, id) in before.core.exact_admissions.iter() {
            if index.get(key) != Some(id) && !index.covers(key) {
                return Err(crate::AdmissionError::rejected(
                    neighbour_wire::RefusalReason::Conflict,
                    "admission candidate discards exact evidence",
                ));
            }
        }
        for (writer, mark) in before.core.exact_admissions.marks() {
            if !index.covers(&writer.at(mark)) {
                return Err(crate::AdmissionError::rejected(
                    neighbour_wire::RefusalReason::Conflict,
                    "admission candidate lowers a compaction mark",
                ));
            }
        }
        Ok(self
            .core
            .exact_admissions
            .iter()
            .filter(|(key, _)| {
                before.core.exact_admissions.get(key).is_none()
                    && !before.core.exact_admissions.covers(key)
            })
            .map(|(key, id)| DocumentAdmission {
                key: key.clone(),
                operation_id: *id,
            })
            .collect())
    }

    /// Admit changes to the workspace's document
    /// ([`Self::receive_workspace_changes`]). A device that could not decide
    /// the batch has not refused it.
    fn receive_workspace_batch(
        &mut self,
        source: [u8; 32],
        received: &str,
        workspace_changes: &[Vec<u8>],
        now: SystemTime,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        self.receive_workspace_changes(source, received, workspace_changes, now, strict_report)
    }
    pub(crate) fn receive(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        self.receive_with_context(source, message, now, false, false)
            .map(|report| report.summary)
            .map_err(err)
    }
    /// The device a frame came from, and whether it came as part of what
    /// another device retains rather than as live traffic.
    pub(super) fn resolve_origin(
        origin: TransportOrigin,
        message: &NeighbourMsg,
        _now: SystemTime,
    ) -> Result<([u8; 32], bool), AdmissionError> {
        match origin {
            TransportOrigin::ServerDelta(source) => Ok((source, false)),
            TransportOrigin::RetainedServerDelta(source) => Ok((source, true)),
            TransportOrigin::RetainedNativePublication { .. } => Err(AdmissionError::malformed(
                "a retained native publication is no longer a carrier",
            )),
            TransportOrigin::UntrustedPresence(source) => {
                if matches!(
                    message,
                    NeighbourMsg::SyncResp { .. } | NeighbourMsg::AppDocument { .. }
                ) {
                    return Err(AdmissionError::malformed(
                        "state requires server-authenticated Delta carrier",
                    ));
                }
                Ok((source, false))
            }
        }
    }

    /// A device's hello: a placed device's, decided by its certificate alone, or a member's.
    fn receive_hello(
        &mut self,
        source: [u8; 32],
        protocol: u32,
        cert: &DeviceCert,
        revocations: Vec<tilo_user_identity::RevocationEntry>,
        now: SystemTime,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        // A device at a peer protocol version this one does not work
        // with is admitted to nothing: what it says is not read here.
        if !neighbour_wire::works_with_peer_version(protocol) {
            return Err(AdmissionError::malformed(format!(
                "the device speaks peer protocol {protocol}, this one {}",
                neighbour_wire::PEER_PROTOCOL_VERSION
            )));
        }
        if cert.capabilities.records.is_some() {
            let mut report = self.receive_placed_device_hello(source, cert, revocations, now)?;
            // A write the device sent before its certificate arrived waited for it.
            if report.summary.metadata_changed && !self.core.pending.is_empty() {
                self.retry_held(&mut report, now, strict_report);
            }
            return Ok(report);
        }
        self.receive_identity_hello(source, cert, revocations, now)
    }

    pub(super) fn receive_with_context(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        retained_replay: bool,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        match message {
            // A placed device is decided against the workspace's members: it waits for the
            // rules that name them.
            message @ NeighbourMsg::IdentityHello { .. }
                if self.core.workspace_doc.is_none()
                    && matches!(
                        &message,
                        NeighbourMsg::IdentityHello { cert, .. }
                            if cert.capabilities.records.is_some()
                    ) =>
            {
                self.defer(
                    source,
                    message,
                    Held {
                        retained_replay,
                        prerequisite: MissingPrerequisite::Rules,
                        now,
                    },
                )
            }
            NeighbourMsg::IdentityHello {
                protocol,
                cert,
                revocations,
                ..
            } => self.receive_hello(source, protocol, &cert, revocations, now, strict_report),
            NeighbourMsg::SyncResp {
                changes,
                workspace: received,
            } => self.receive_workspace_batch(source, &received, &changes, now, strict_report),
            message @ NeighbourMsg::AppDocument { .. } => {
                if self.core.workspace_doc.is_none() {
                    return self.defer(
                        source,
                        message,
                        Held {
                            retained_replay,
                            prerequisite: MissingPrerequisite::Rules,
                            now,
                        },
                    );
                }
                // A placed device is no member: its certificate alone decides what it writes
                // ([`Self::written_as`]).
                if self.core.placed_devices.contains_key(&source) {
                    return self.receive_document(
                        source,
                        message,
                        now,
                        retained_replay,
                        strict_report,
                    );
                }
                // A device the roster does not certify writes nothing here
                // yet: its write is held until the governance listing it
                // arrives, and applied only once the workspace lists it. A
                // holder that is no member never is.
                if !self.core.certificates.contains_key(&source) {
                    return self.defer(
                        source,
                        message,
                        Held {
                            retained_replay,
                            prerequisite: MissingPrerequisite::Certificate,
                            now,
                        },
                    );
                }
                if matches!(
                    backend::member::role(&self.core, &source, now),
                    Err(MembershipError::Absent)
                ) {
                    // The governance that lists the writer's device has not arrived yet: a
                    // replay or a delivery ahead of it. A write it would refuse is held and
                    // decided again with every governance change; it is applied only once the
                    // workspace lists its writer.
                    return match self.receive_document(
                        source,
                        message.clone(),
                        now,
                        retained_replay,
                        strict_report,
                    ) {
                        Err(_) => tilo_failure::ignored!(
                            "the write is held and decided again, not refused",
                            self.defer(
                                source,
                                message,
                                Held {
                                    retained_replay,
                                    prerequisite: MissingPrerequisite::Rules,
                                    now,
                                },
                            )
                        ),
                        decided => decided,
                    };
                }
                self.receive_document(source, message, now, retained_replay, strict_report)
            }
            _ => Ok(AdmissionReport::new(
                InputDisposition::UnsupportedFamily,
                ReceiveOutcome::default(),
            )),
        }
    }
    /// Admit the transport device's own certificate and the revocations its
    /// identity root signed.
    fn receive_identity_hello(
        &mut self,
        source: [u8; 32],
        cert: &DeviceCert,
        revocations: Vec<tilo_user_identity::RevocationEntry>,
        now: SystemTime,
    ) -> Result<AdmissionReport, AdmissionError> {
        if cert.device_public_key != source {
            return Err(crate::AdmissionError::forbidden(
                "transport certificate mismatch",
            ));
        }
        if self.core.certificates.get(&source).map(AsRef::as_ref) != Some(cert) {
            let admitted = self.core.workspace_doc.as_ref().and_then(|doc| {
                doc.identities()
                    .get(&hex::encode(source))
                    .and_then(|identity| identity.as_ref().ok())
            });
            if admitted.is_none_or(|identity| identity.device_cert != *cert) {
                return Err(crate::AdmissionError::rejected(
                    neighbour_wire::RefusalReason::NotCertified,
                    "identity is absent from admitted roster",
                ));
            }
        }
        let verified = VerifiedCertificate::new(cert.clone(), now_ns(now))
            .map_err(crate::AdmissionError::malformed)?;
        let mut metadata_changed =
            self.core.certificates.get(&source).map(AsRef::as_ref) != Some(cert);
        for entry in revocations {
            if entry.user_public_key != cert.user_public_key {
                return Err(crate::AdmissionError::forbidden("revocation root mismatch"));
            }
            if self
                .core
                .certificates
                .get(&entry.revoked_device_public_key)
                .is_some_and(|target| target.user_public_key != entry.user_public_key)
            {
                return Err(crate::AdmissionError::forbidden(
                    "revocation cannot cross identity roots",
                ));
            }
            metadata_changed |= self
                .core
                .revocations
                .insert(entry)
                .map_err(crate::AdmissionError::malformed)?;
        }
        if self
            .core
            .revocations
            .is_revoked(&cert.user_public_key, &source)
        {
            return Err(crate::AdmissionError::rejected(
                neighbour_wire::RefusalReason::Revoked,
                "device revoked",
            ));
        }
        self.core.certificates.insert(source, verified);
        Ok(AdmissionReport::metadata(ReceiveOutcome {
            metadata_changed,
            ..Default::default()
        }))
    }
    /// Take in the certificate of `source`, a device placed for records of this
    /// workspace alone ([`tilo_user_identity::DeviceCapabilities::records`]),
    /// and the revocations its person's root signed. It is kept apart from the
    /// workspace's roster and from every list of its devices: the device is no
    /// member, and its certificate is all that decides what it writes and
    /// reads. Taken while its chain verifies, it names this workspace, the
    /// person it acts as is one of the workspace's, and neither it nor the
    /// host that issued it is revoked.
    fn receive_placed_device_hello(
        &mut self,
        source: [u8; 32],
        cert: &DeviceCert,
        revocations: Vec<tilo_user_identity::RevocationEntry>,
        now: SystemTime,
    ) -> Result<AdmissionReport, AdmissionError> {
        if cert.device_public_key != source {
            return Err(crate::AdmissionError::forbidden(
                "transport certificate mismatch",
            ));
        }
        if cert.capabilities.placed_in(&self.core.workspace).is_none() {
            return Err(crate::AdmissionError::rejected(
                neighbour_wire::RefusalReason::NotCertified,
                "the device was placed for records of another workspace",
            ));
        }
        if self.core.certificates.contains_key(&source) {
            return Err(crate::AdmissionError::forbidden(
                "a device of the workspace's roster is placed for no records",
            ));
        }
        let verified = VerifiedCertificate::new(cert.clone(), now_ns(now))
            .map_err(crate::AdmissionError::malformed)?;
        let Some(rules) = self.core.workspace_doc.as_ref() else {
            return Err(crate::AdmissionError::malformed(
                "a placed device is decided against the workspace's rules",
            ));
        };
        let person = hex::encode(cert.user_public_key);
        if backend::member::role_in(
            rules.workspace_policy(&self.core.workspace),
            &self.core.workspace,
            &person,
        )
        .is_none()
        {
            return Err(crate::AdmissionError::rejected(
                neighbour_wire::RefusalReason::Forbidden,
                "the person the device acts as is no member of the workspace",
            ));
        }
        let mut metadata_changed =
            self.core.placed_devices.get(&source).map(AsRef::as_ref) != Some(cert);
        for entry in revocations {
            if entry.user_public_key != cert.user_public_key {
                return Err(crate::AdmissionError::forbidden("revocation root mismatch"));
            }
            metadata_changed |= self
                .core
                .revocations
                .insert(entry)
                .map_err(crate::AdmissionError::malformed)?;
        }
        // Kept even when it is refused now, so what the device sends is refused as its
        // certificate's, never deferred as an unknown device's.
        self.core.placed_devices.insert(source, verified);
        if let Err(refused) = self.core.placed_certificate(&source, now) {
            return Err(crate::AdmissionError::rejected(
                MembershipError::from(refused).refusal_reason(),
                "the device's certificate or the host that issued it is revoked",
            ));
        }
        Ok(AdmissionReport::metadata(ReceiveOutcome {
            metadata_changed,
            ..Default::default()
        }))
    }

    /// Admit governance changes to the workspace document, then retry every input
    /// deferred until governance arrived.
    fn receive_workspace_changes(
        &mut self,
        source: [u8; 32],
        received: &str,
        workspace_changes: &[Vec<u8>],
        now: SystemTime,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        if received != self.core.workspace {
            return Err(crate::AdmissionError::forbidden("workspace topic mismatch"));
        }
        let access = self
            .workspace_writer(source, now)
            .map_err(crate::AdmissionError::forbidden)?;
        let owner = self.core.owner;
        let administrators = self.administrator_roots();
        let from = hex::encode(source);
        let anchors: Vec<[u8; 32]> = self
            .own_device
            .into_iter()
            .chain([self.core.origin])
            .collect();
        // The one workspace admission the shell's workspace tree runs too.
        let workspace::AdmittedWorkspace {
            mut candidate,
            rebased,
        } = workspace::admit_workspace_changes(
            workspace::WorkspaceAdmission {
                before: self.core.workspace_doc.as_deref(),
                workspace: &self.core.workspace,
                source: workspace::Writer { key: &from, access },
                owner: &owner,
                administrators: &administrators,
                anchors: &anchors,
                now_ns: now_ns(now),
            },
            workspace_changes,
        )?;
        if rebased && let Some(before) = self.core.workspace_doc.as_deref() {
            self.carry_over_base(before, &mut candidate)?;
        }
        // The workspace's owner is the person its governance names, never
        // the person of the device this replica was joined through
        // ([`Self::from_join`] names no owner until the governance arrives).
        if self.core.workspace_doc.is_none()
            && let Some(owner) = owner_named_by(&candidate, &self.core.workspace)
        {
            self.core.owner = owner;
        }
        // Receipts this peer admitted earlier stay immutable even once
        // the workspace document omits them.
        let mut receipts = self.core.receipts.clone();
        workspace::admit_receipts(&mut receipts, &candidate)
            .map_err(crate::AdmissionError::malformed)?;
        let changed = rebased
            || self
                .core
                .workspace_doc
                .as_deref()
                .is_none_or(|doc| doc.base_heads() != candidate.base_heads());
        let lacks_history = self.newly_waiting(&candidate);
        // The rows the change left as they were keep what reading them took.
        let candidate = match self.core.workspace_doc.as_ref() {
            Some(held) => held.followed_by(candidate),
            None => candidate.into(),
        };
        // The owner is whom the governance names now: a transfer the batch
        // carried makes its person the owner here as on every device that
        // replays it, and the certificates are taken in under that owner. A
        // state that names no owner, or another one without the transfer that
        // hands it over, is no state of this workspace: taking it in would
        // make the person this replica knows as the owner one of its members
        // instead.
        let held_owner = self.core.owner;
        let first = self.core.workspace_doc.is_none();
        let Some(owner) = listed_owner(&candidate, &self.core.workspace) else {
            return Err(crate::AdmissionError::malformed(
                "the workspace's governance names no owner",
            ));
        };
        if !first && owner != held_owner && !candidate.owner_moved(&self.core.workspace) {
            return Err(crate::AdmissionError::malformed(
                "the workspace's governance names an owner it was not handed to",
            ));
        }
        self.core.owner = owner;
        let metadata_changed = match self.import_device_rows(&candidate, now) {
            Ok(changed) => changed || self.core.owner != held_owner,
            Err(error) => {
                self.core.owner = held_owner;
                return Err(crate::AdmissionError::malformed(error));
            }
        };
        self.core.receipts = receipts;
        self.core.workspace_doc = Some(candidate);
        // The governance may name a record this replica holds nothing of and
        // that no one can send it: an empty one. Held before the writes
        // waiting on it are tried, so the first one builds on the record.
        covered_documents::hold_covered_empty_documents(self)
            .map_err(crate::AdmissionError::malformed)?;
        let mut report = AdmissionReport::metadata(ReceiveOutcome {
            changed,
            metadata_changed,
            lacks_history,
            ..Default::default()
        });
        self.retry_held(&mut report, now, strict_report);
        report.incomplete = self.incomplete_reason();
        Ok(report)
    }
    /// What this copy's device wrote against the history the base replaced,
    /// and the base does not hold, it writes again on the base; and the copy
    /// keeps what it saw, the forms the base lacks going back into their rows
    /// as the keeper's own change, before anything reads the adopted document.
    fn carry_over_base(
        &self,
        before: &automerge::AutoCommit,
        candidate: &mut automerge::AutoCommit,
    ) -> Result<(), AdmissionError> {
        if let Some(device) = self.own_device {
            // The change is written on the base this copy adopts, as that base's generation
            // numbers it: the history the base replaced gave this device other numbers, and the
            // base names what it holds of those.
            let held_as =
                self.workspace_actor_in(&device, crate::workspace::base::generation(before));
            let actor =
                self.workspace_actor_in(&device, crate::workspace::base::generation(candidate));
            workspace::base::carried(before, candidate, (&held_as, &actor))
                .map_err(crate::AdmissionError::malformed)?;
        }
        Ok(())
    }

    /// Who writes the workspace's changes as `source`'s device: the member it
    /// is, holding a role on the signed member list as the workspace's owner
    /// does too, else a device enrolling itself, which writes its own
    /// admission and nothing else. A device the workspace revoked, or took off
    /// its member list with the mark that removed it
    /// ([`crate::PeerGovernance::removed_from_member_list`]), writes nothing,
    /// and is told so.
    fn workspace_writer(
        &self,
        source: [u8; 32],
        now: SystemTime,
    ) -> Result<workspace::SenderAccess, String> {
        let held = self
            .certificate(&source, now)
            .and_then(|cert| self.role(&source, now).map(|_| cert));
        if let Err(revoked @ super::MembershipError::Revoked) = &held {
            return Err(revoked.to_string());
        }
        if let Ok(cert) = &held
            && self.governance().removed_from_member_list(cert)
        {
            return Err(super::MembershipError::Absent.to_string());
        }
        let member = tilo_failure::ignored!(
            "a sender the workspace does not list with a role yet writes only its own admission",
            held.ok()
        );
        Ok(member.map_or(workspace::SenderAccess::Enrolling, |cert| {
            workspace::SenderAccess::Member {
                person: cert.user_public_key,
                foreign: backend::member::foreign(cert, &self.core.origin),
            }
        }))
    }

    pub(super) fn receive_document(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        retained_replay: bool,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        let written = written_route(&message);
        // A write that is one part of an action across several records is taken in with the
        // action's other parts ([`super::action_parts`]).
        let mut outcome = match super::action_parts::action_of(&message) {
            Ok(Some(commit)) => self.admit_action_part(
                source,
                message,
                &commit,
                now,
                retained_replay,
                strict_report,
            ),
            Ok(None) => {
                self.admit_document(source, message, now, false, retained_replay, strict_report)
            }
            Err(error) => Err(error),
        }
        .inspect_err(|_| self.forget_refused(source, written.as_ref()))?;
        if let (
            Some((route, sequence)),
            InputDisposition::Accepted(_) | InputDisposition::MetadataOnly,
        ) = (&written, &outcome.primary)
            && !outcome.deferred
            && !outcome.duplicate
        {
            let let_go = self.let_go_overtaken_actions(route, source, *sequence);
            outcome.summary.dropped.extend(let_go);
        }
        if outcome.deferred || self.core.pending.is_empty() {
            return Ok(outcome);
        }
        self.retry_held(&mut outcome, now, strict_report);
        Ok(outcome)
    }
}

/// The owner `document`'s governance names now, as their root key: the
/// founding owner, or whom the last transfer it replays handed it to.
pub(super) fn listed_owner(document: &super::CachedDocument, workspace: &str) -> Option<[u8; 32]> {
    let owner = document.workspace_policy(workspace).owner()?;
    let mut key = [0; 32];
    hex::decode_to_slice(owner, &mut key).ok()?;
    Some(key)
}

/// The owner `document`'s governance names, as their root key.
pub(super) fn owner_named_by(
    document: &automerge::AutoCommit,
    workspace: &str,
) -> Option<[u8; 32]> {
    let owner: [u8; 32] = hex::decode(workspace::policy_of(document, workspace).owner()?)
        .ok()?
        .try_into()
        .ok()?;
    Some(owner)
}
