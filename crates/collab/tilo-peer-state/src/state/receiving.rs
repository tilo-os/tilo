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
    pub fn receive_from_report(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<AdmissionReport, AdmissionError> {
        if matches!(message, NeighbourMsg::AppDocument { .. }) && !self.core.exact_selected {
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
        let received = candidate.receive_with_context(source, message, now, retained_replay, true);
        self.remember_refused_base(&candidate);
        let mut report = received.inspect_err(|_| self.forget_refused(source, written.as_ref()))?;
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
            document_id,
            envelope,
            ..
        } = message
        else {
            return Err("exact document lookup requires AppDocument".into());
        };
        if *peer != peer_handle(&source) {
            return Err("transport author mismatch".into());
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
            return Err("transport author or document workspace mismatch".into());
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
                    denied @ super::authority::AuthorizeRefusal::Denied(_) => {
                        AdmissionError::Rejected(denied.to_string())
                    }
                })?,
            super::authority::Authorized::Hold(_)
        ) {
            // Held for what it waits for: decided by the real admission, never a recorded one.
            return Ok(None);
        }
        collab_types::validate_whole_document_payload_route(app, workspace, document, op)
            .map_err(err)?;
        collab_types::verify_op(op).map_err(err)?;
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
                {
                    Ok(None)
                } else {
                    Ok(Some(admission))
                }
            }
            None => Ok(None),
        }
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
                .and_then(|doc| governed_owner(doc, &self.core.workspace))
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
    /// An [`AdmissionError::Device`] is this device failing to decide it, never
    /// the message being refused.
    pub fn receive_from(
        &mut self,
        origin: TransportOrigin,
        message: NeighbourMsg,
        now: SystemTime,
    ) -> Result<ReceiveOutcome, AdmissionError> {
        if let TransportOrigin::RetainedNativePublication { publisher } = origin {
            return self
                .receive_native_publication(publisher, message, now, false)
                .map(|report| report.summary);
        }
        let (source, retained_replay) = self.resolve_origin(origin, &message, now)?;
        let written = written_route(&message);
        let mut candidate = self.clone();
        let received = candidate.receive_with_context(source, message, now, retained_replay, false);
        self.remember_refused_base(&candidate);
        let mut outcome = received.inspect_err(|error| {
            // A device that could not decide the frame has not refused it:
            // what waited for the writer stays waiting for it.
            if !error.is_device_failure() {
                self.forget_refused(source, written.as_ref());
            }
        })?;
        let landed = outcome.summary.changed || outcome.summary.metadata_changed;
        outcome.summary.replay_wanted |= candidate.missed_landed(landed);
        *self = candidate;
        Ok(outcome.summary)
    }
    /// Keep what `candidate`, this copy's own clone, learnt while it decided
    /// a message: the base of its governor it refused. A refused message
    /// leaves nothing else of itself here.
    fn remember_refused_base(&mut self, candidate: &Self) {
        self.refused_base = self.refused_base.max(candidate.refused_base);
    }
    /// Admit changes to the workspace's document
    /// ([`Self::receive_workspace_changes`]), and note a newer base of its
    /// governor that this copy refuses: governance the copy then knows of
    /// and does not hold. Until it holds a base that new, it takes in no
    /// write on the roles and grants it holds
    /// ([`Self::behind_its_governor`]). A device that could not decide the
    /// batch has not refused it.
    fn receive_workspace_batch(
        &mut self,
        source: [u8; 32],
        received: &str,
        workspace_changes: &[Vec<u8>],
        now: SystemTime,
        strict_report: bool,
    ) -> Result<AdmissionReport, AdmissionError> {
        let decided =
            self.receive_workspace_changes(source, received, workspace_changes, now, strict_report);
        if decided
            .as_ref()
            .is_err_and(|error| !error.is_device_failure())
            && self
                .certificate(&source, now)
                .is_ok_and(|cert| self.governs_history(&cert.user_public_key))
            && let workspace::base::Lineage::Newer { base, .. } =
                workspace::base::lineage(self.core.workspace_doc.as_deref(), workspace_changes)
        {
            self.refused_base = self
                .refused_base
                .max(Some(workspace::base::generation(&base)));
        }
        decided
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
                // A device at a peer protocol version this one does not work
                // with is admitted to nothing: what it says is not read here.
                if !neighbour_wire::works_with_peer_version(protocol) {
                    return Err(AdmissionError::Rejected(format!(
                        "the device speaks peer protocol {protocol}, this one {}",
                        neighbour_wire::PEER_PROTOCOL_VERSION
                    )));
                }
                self.receive_identity_hello(source, &cert, revocations, now)
            }
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
                            prerequisite: MissingPrerequisite::Governance,
                            now,
                        },
                    );
                }
                // A holder of the workspace is known by the address it signed
                // among the workspace's holders, whether or not the roster
                // carries its certificate: what it writes is decided now, as a
                // member's work under a member's delegation or not at all. A
                // holder never becomes a member by governance arriving later.
                let holder = self.holder_certificate(&source).is_some();
                if !holder && !self.core.certificates.contains_key(&source) {
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
                if !holder
                    && matches!(
                        backend::member::legacy(&self.core, &source, now),
                        Err(MembershipError::Absent)
                    )
                {
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
                                    prerequisite: MissingPrerequisite::Governance,
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
        let access = self.workspace_writer(source, now)?;
        let owner = self.core.owner;
        let administrators = self.administrator_roots();
        let from = hex::encode(source);
        let keeper = self.workspace_keeper(now);
        // The one workspace admission the shell's workspace tree runs too.
        let workspace::AdmittedWorkspace {
            mut candidate,
            media,
            rebased,
        } = workspace::admit_workspace_changes(
            workspace::WorkspaceAdmission {
                before: self.core.workspace_doc.as_deref(),
                workspace: &self.core.workspace,
                source: workspace::Writer { key: &from, access },
                owner: &owner,
                administrators: &administrators,
                now_ns: now_ns(now),
                keeps_delegations: keeper.is_some(),
            },
            workspace_changes,
        )?;
        if rebased && let Some(before) = self.core.workspace_doc.as_deref() {
            self.carry_over_base(before, keeper, &mut candidate)?;
        }
        // The workspace's owner is the person its governance names, not the
        // person of the device this replica was joined through: a member's
        // device admits its person's further device too, and the workspace
        // stays its own owner's ([`Self::from_join`] names the admitting
        // device until the governance arrives).
        if self.core.workspace_doc.is_none()
            && let Some(owner) = owner_named_by(&candidate, &self.core.workspace)
        {
            self.core.owner = owner;
        }
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
        // The owner is whom the governance names now: a transfer the batch
        // carried makes its person the owner here as on every device that
        // replays it, and the certificates are taken in under that owner. A
        // state that names no owner, or another one without the transfer that
        // hands it over, is no state of this workspace: taking it in would
        // make the person this replica knows as the owner one of its members
        // instead.
        let held_owner = self.core.owner;
        let first = self.core.workspace_doc.is_none();
        let Some(owner) = governed_owner(&candidate, &self.core.workspace) else {
            return Err("the workspace's governance names no owner".into());
        };
        if !first && owner != held_owner && !candidate.owner_moved(&self.core.workspace) {
            return Err("the workspace's governance names an owner it was not handed to".into());
        }
        self.core.owner = owner;
        let metadata_changed = match self.import_governed_rows(&candidate, now) {
            Ok(changed) => changed || self.core.owner != held_owner,
            Err(error) => {
                self.core.owner = held_owner;
                return Err(error.into());
            }
        };
        self.core.receipts = receipts;
        self.core.workspace_doc = Some(candidate);
        // The governance may name a record this replica holds nothing of and
        // that no one can send it: an empty one. Held before the writes
        // waiting on it are tried, so the first one builds on the record.
        covered_documents::hold_covered_empty_documents(self)?;
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
    /// What this copy's device wrote against the history the base replaced,
    /// and the base does not hold, it writes again on the base; and the copy
    /// keeps what it saw, the forms the base lacks going back into their rows
    /// as the keeper's own change, before anything reads the adopted document.
    fn carry_over_base(
        &self,
        before: &automerge::AutoCommit,
        keeper: Option<[u8; 32]>,
        candidate: &mut automerge::AutoCommit,
    ) -> Result<(), AdmissionError> {
        if let Some(device) = self.own_device {
            let actor = self.workspace_actor_for(&device);
            workspace::base::carried(before, candidate, &actor).map_err(err)?;
        }
        if let Some(device) = keeper {
            let actor = self.workspace_actor_for(&device);
            super::work_delegations::carried(before, candidate, &actor)?;
        }
        Ok(())
    }

    /// Who writes the workspace's changes as `source`'s device: the member it
    /// is, else one of the workspace's holders by the address it signed.
    ///
    /// The owner's devices write governance on their certificate alone: the
    /// device this replica was joined through supplies the first governance
    /// before any roster exists.
    fn workspace_writer(
        &self,
        source: [u8; 32],
        now: SystemTime,
    ) -> Result<workspace::SenderAccess, String> {
        let member = self
            .certificate(&source, now)
            .map_err(|error| error.to_string())
            .and_then(|cert| {
                let person = cert.user_public_key;
                let foreign = backend::member::foreign(cert, &self.core.origin);
                if person != self.core.owner {
                    self.role(&source, now).map_err(|error| error.to_string())?;
                }
                Ok(workspace::SenderAccess::Member { person, foreign })
            });
        match member {
            Ok(access) => Ok(access),
            Err(error) => self
                .holder_certificate(&source)
                .map(|cert| workspace::SenderAccess::Holder {
                    person: cert.user_public_key,
                })
                .ok_or(error),
        }
    }

    /// This copy's own device, when it knows it and it is a member whose work
    /// writes here: such a copy keeps the work delegations it holds where a
    /// base lacks them.
    fn workspace_keeper(&self, now: SystemTime) -> Option<[u8; 32]> {
        let owner = self.core.owner;
        self.own_device.filter(|device| {
            self.certificate(device, now).is_ok_and(|cert| {
                let access = workspace::SenderAccess::Member {
                    person: cert.user_public_key,
                    foreign: backend::member::foreign(cert, &self.core.origin),
                };
                workspace::writes_workspace(
                    self.core.workspace_doc.as_deref(),
                    &self.core.workspace,
                    &workspace::Writer {
                        key: &hex::encode(device),
                        access,
                    },
                    &owner,
                    now_ns(now),
                )
            })
        })
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

/// The owner `document`'s governance names now, as their root key: the
/// founding owner, or whom the last transfer it replays handed it to.
pub(super) fn governed_owner(
    document: &super::CachedDocument,
    workspace: &str,
) -> Option<[u8; 32]> {
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
