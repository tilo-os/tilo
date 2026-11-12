//! Taking in what other devices send: workspace changes, record writes, hellos.

use super::{
    AdmissionError, AdmissionReport, DeviceCert, DocumentAdmission, Held, InputDisposition,
    MembershipError, MissingPrerequisite, NeighbourMsg, ReceiveOutcome, SignedOp, StatePeer,
    SystemTime, TransportOrigin, VerifiedCertificate, backend, covered_records, err, now_ns,
    peer_handle, workspace, written_route,
};

use automerge::transaction::Transactable as _;

impl StatePeer {
    /// Decide received changes by their claims against the app surfaces
    /// `surfaces` finds.
    pub fn set_surfaces(&mut self, surfaces: collab_types::Surfaces) {
        self.surfaces = surfaces;
    }

    /// Select native schema v5 only as part of an outer owner's reviewed rollout.
    /// This selects candidate serialization; it does not commit any bytes.
    pub fn select_admission_v5(&mut self) -> Result<(), AdmissionError> {
        self.core.exact_admissions.encode()?;
        self.core.admission_v5 = true;
        Ok(())
    }
    pub fn admission_v5_selected(&self) -> bool {
        self.core.admission_v5
    }

    /// Exact observations from the real admission path, not a durability receipt.
    pub fn receive_from_report(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<AdmissionReport, AdmissionError> {
        if matches!(message, NeighbourMsg::AppDocument { .. }) && !self.core.admission_v5 {
            return Err(AdmissionError::WriterSelectionRequired);
        }
        if let TransportOrigin::RetainedNativePublication { publisher } = origin {
            return self.receive_native_publication(publisher, message, now, true);
        }
        if matches!(&message, NeighbourMsg::AppDocument { .. })
            && let Some(report) = self.lookup_exact_document_report(origin, &message, now)?
        {
            return Ok(report);
        }
        let (source, retained_replay) = self.resolve_origin(origin, &message, now)?;
        let written = written_route(&message);
        let mut candidate = self.clone();
        let mut report = candidate
            .receive_with_context(source, message, now, retained_replay, true)
            .inspect_err(|_| self.forget_refused(source, written.as_ref()))?;
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
        if !self.core.admission_v5 {
            return Err(AdmissionError::WriterSelectionRequired);
        }
        if let Some(publisher) = self.core.native_cut_publisher {
            self.validate_native_publisher(publisher, now)?;
        }
        let (source, _) = self.resolve_origin(origin, message, now)?;
        if self.core.workspace_doc.is_none() || !self.core.certificates.contains_key(&source) {
            return Ok(None);
        }
        // A write whose writer this peer's governance does not list yet is no recorded
        // duplicate: the real admission holds it.
        if matches!(
            backend::member::legacy(&self.core, &source, now),
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
        let (source, _) = self.resolve_origin(origin, message, now)?;
        let NeighbourMsg::AppDocument {
            peer,
            app_id,
            cluster_id,
            record_id,
            envelope,
        } = message
        else {
            return Err("exact document lookup requires AppDocument".into());
        };
        if *peer != peer_handle(&source) {
            return Err("transport author mismatch".into());
        }
        self.lookup_exact_document_parts(source, app_id, cluster_id, record_id, envelope, now)
    }
    pub(super) fn lookup_exact_document_parts(
        &self,
        source: [u8; 32],
        app: &str,
        workspace: &str,
        record: &str,
        op: &SignedOp,
        now: SystemTime,
    ) -> Result<Option<DocumentAdmission>, AdmissionError> {
        if workspace != self.core.workspace || op.author.as_bytes() != &source {
            return Err("transport author or document workspace mismatch".into());
        }
        if self
            .authorize_document(&source, app, record, op, now)?
            .is_none()
        {
            // Held for its app surface: decided by the real admission, never a recorded one.
            return Ok(None);
        }
        collab_types::validate_whole_document_payload_route(app, workspace, record, op)
            .map_err(err)?;
        collab_types::verify_op(op).map_err(err)?;
        let admission = DocumentAdmission::from_op(workspace, app, record, op)?;
        match self.core.exact_admissions.get(&admission.key) {
            Some(id) if id == &admission.operation_id => Ok(Some(admission)),
            Some(_) => Err(AdmissionError::ConflictingSequence),
            // A compaction folded it: decided, and ignored like a retry.
            None if self.core.exact_admissions.covers(&admission.key) => Ok(Some(admission)),
            None => Ok(None),
        }
    }

    /// Exact new candidate admissions for local multi-record owner commits.
    /// Also refuses replacement that discards prior exact evidence.
    pub fn admissions_since(
        &self,
        before: &StatePeer,
    ) -> Result<Vec<DocumentAdmission>, AdmissionError> {
        if self.core.workspace != before.core.workspace
            || self.core.owner != before.core.owner
            || (before.core.admission_v5 && !self.core.admission_v5)
        {
            return Err(
                "admission candidate replaces namespace or downgrades native writer".into(),
            );
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
                    return Err("admission candidate discards exact evidence".into());
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
                return Err("admission candidate discards exact evidence".into());
            }
        }
        for (writer, mark) in before.core.exact_admissions.marks() {
            if !index.covers(&writer.at(mark)) {
                return Err("admission candidate lowers a compaction mark".into());
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

    /// Admit a message from `origin`, queuing it while prerequisites are missing.
    pub fn receive_from(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, String> {
        if let TransportOrigin::RetainedNativePublication { publisher } = origin {
            return self
                .receive_native_publication(publisher, message, now, false)
                .map(|report| report.summary)
                .map_err(err);
        }
        let (source, retained_replay) = self.resolve_origin(origin, &message, now).map_err(err)?;
        let written = written_route(&message);
        let mut candidate = self.clone();
        let mut outcome = candidate
            .receive_with_context(source, message, now, retained_replay, false)
            .map_err(|error| {
                self.forget_refused(source, written.as_ref());
                err(error)
            })?;
        let landed = outcome.summary.changed || outcome.summary.metadata_changed;
        outcome.summary.replay_wanted |= candidate.missed_landed(landed);
        *self = candidate;
        Ok(outcome.summary)
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
    pub(super) fn receive_with_context(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        now: SystemTime,
        retained_replay: bool,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        if let Some(publisher) = self.core.native_cut_publisher {
            self.validate_native_publisher(publisher, now)?;
        }
        match message {
            NeighbourMsg::IdentityHello {
                protocol,
                cert,
                revocations,
                ..
            } => {
                // A device at another peer protocol version is admitted to
                // nothing: what it says is not read at this one.
                if protocol != neighbour_wire::PEER_PROTOCOL_VERSION {
                    return Err(AdmissionError::Rejected(format!(
                        "the device speaks peer protocol {protocol}, this one {}",
                        neighbour_wire::PEER_PROTOCOL_VERSION
                    )));
                }
                self.receive_identity_hello(source, &cert, revocations, now)
            }
            NeighbourMsg::SyncResp {
                changes,
                room: received,
            } => self.receive_workspace_changes(source, &received, &changes, now, strict_report),
            message @ NeighbourMsg::AppDocument { .. } => {
                if self.core.workspace_doc.is_none()
                    || !self.core.certificates.contains_key(&source)
                {
                    let prerequisite = if self.core.workspace_doc.is_none() {
                        MissingPrerequisite::Governance
                    } else {
                        MissingPrerequisite::Certificate
                    };
                    return self.defer(
                        source,
                        message,
                        Held {
                            retained_replay,
                            prerequisite,
                            now,
                        },
                    );
                }
                if matches!(
                    backend::member::legacy(&self.core, &source, now),
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
                        // ignored-failure: the write is held and decided again, not refused
                        Err(_) => self.defer(
                            source,
                            message,
                            Held {
                                retained_replay,
                                prerequisite: MissingPrerequisite::Governance,
                                now,
                            },
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
            return Err("transport certificate mismatch".into());
        }
        if self.core.certificates.get(&source).map(AsRef::as_ref) != Some(cert) {
            let admitted = self.core.workspace_doc.as_ref().and_then(|doc| {
                doc.identities()
                    .get(&hex::encode(source))
                    .and_then(|identity| identity.as_ref().ok())
            });
            if admitted.is_none_or(|identity| identity.device_cert != *cert) {
                return Err("identity is absent from admitted roster".into());
            }
        }
        let verified = VerifiedCertificate::new(cert.clone(), now_ns(now))?;
        let mut metadata_changed =
            self.core.certificates.get(&source).map(AsRef::as_ref) != Some(cert);
        for entry in revocations {
            if entry.user_public_key != cert.user_public_key {
                return Err("revocation root mismatch".into());
            }
            if self
                .core
                .certificates
                .get(&entry.revoked_device_public_key)
                .is_some_and(|target| target.user_public_key != entry.user_public_key)
            {
                return Err("revocation cannot cross identity roots".into());
            }
            metadata_changed |= self.core.revocations.insert(entry).map_err(err)?;
        }
        if self
            .core
            .revocations
            .is_revoked(&cert.user_public_key, &source)
        {
            return Err("device revoked".into());
        }
        self.core.certificates.insert(source, verified);
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
            return Err("workspace topic mismatch".into());
        }
        let cert = self
            .certificate(&source, now)
            .map_err(|error| error.to_string())?;
        let person = cert.user_public_key;
        let foreign = backend::member::foreign(cert, &self.core.origin);
        // The owner's devices write governance on their certificate alone:
        // the device this replica was joined through supplies the first
        // governance before any roster exists.
        if person != self.core.owner {
            self.role(&source, now)?;
        }
        let owner = self.core.owner;
        let from = hex::encode(source);
        // The one workspace admission the shell's workspace tree runs too.
        let workspace::AdmittedWorkspace {
            candidate,
            media,
            rebased,
        } = workspace::admit_workspace_changes(
            workspace::WorkspaceAdmission {
                before: self.core.workspace_doc.as_deref(),
                workspace: &self.core.workspace,
                source: workspace::Writer {
                    key: &from,
                    standing: workspace::Standing::Member { person, foreign },
                },
                owner: &owner,
                now_ns: now_ns(now),
            },
            workspace_changes,
        )
        .map_err(err)?;
        // Receipts this peer admitted earlier stay immutable even once
        // the workspace document omits them.
        let mut receipts = self.core.receipts.clone();
        workspace::admit_receipts(&mut receipts, &candidate).map_err(err)?;
        let changed = rebased
            || self
                .core
                .workspace_doc
                .as_deref()
                .is_none_or(|doc| doc.base_heads() != candidate.base_heads());
        let lacks_history = self.newly_waiting(&candidate);
        if let Some(policy) = &media {
            self.validate_media_bindings(policy, false)?;
        }
        // The rows the change left as they were keep what reading them took.
        let candidate = match self.core.workspace_doc.as_ref() {
            Some(held) => held.followed_by(candidate),
            None => candidate.into(),
        };
        let metadata_changed = self.import_governed_rows(&candidate, now)?;
        self.core.receipts = receipts;
        self.core.workspace_doc = Some(candidate);
        // The governance may name a record this replica holds nothing of and
        // that no one can send it: an empty one. Held before the writes
        // waiting on it are tried, so the first one builds on the record.
        covered_records::hold_covered_empty_records(self)?;
        let mut report = AdmissionReport::metadata(ReceiveOutcome {
            changed,
            metadata_changed,
            lacks_history,
            ..Default::default()
        });
        self.retry_held(&mut report, now, strict_report)?;
        report.incomplete = self.incomplete_reason();
        Ok(report)
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
        let mut outcome = self
            .admit_document(source, message, now, false, retained_replay, strict_report)
            .inspect_err(|_| self.forget_refused(source, written.as_ref()))?;
        if outcome.deferred || self.core.pending.is_empty() {
            return Ok(outcome);
        }
        self.retry_held(&mut outcome, now, strict_report)?;
        Ok(outcome)
    }
}
