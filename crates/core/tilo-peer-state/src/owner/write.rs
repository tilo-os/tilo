//! This device's writes to its workspaces' records. Every writer (a page, an agent, the desktop,
//! an app's broker) writes the same way: one turn drafts and commits the write, and the device's
//! signer signs what the turn asked for between its runs (a page's key signs through a promise),
//! so a run that still asked for a signature commits nothing and runs again.
//!
//! A write this device makes is admitted into its own replica, made durable in its record, and
//! kept there until a link carried it to a peer: nothing hands it to a host to be ordered or
//! answered.

use crate::AdmissionError;
use crate::owner::{NativeWorkspaceOwner, NeighbourMsg, SystemTime, WorkspaceId, stored};
use collab_types::signer::DeviceSigner;
use std::sync::Arc;

/// Why the device committed none of a write.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, serde::Serialize, serde::Deserialize)]
pub enum WriteRefusal {
    /// The sealed history of `record` of `app` has no room for the writes, and this device could
    /// not compact it: the writes wait until it can.
    #[error("the sealed history of {app}/{record} is full")]
    HistoryFull { app: String, record: String },
    /// This device's signer did not sign the writes.
    #[error("signing failed: {0}")]
    Unsigned(String),
    /// The workspace has not admitted what the writes need here yet: they wait, kept, and commit
    /// once it has.
    #[error("the writes wait for {0}")]
    Waiting(crate::admission::MissingPrerequisite),
    /// The workspace's record does not take the writes, for a reason this device has no class
    /// for.
    #[error("{0}")]
    Refused(String),
    /// The workspace refused the writes for a reason this device knows: the person is told it,
    /// and `detail` stays for the log.
    #[error("{detail}")]
    Declined {
        reason: neighbour_wire::RefusalReason,
        detail: String,
    },
    /// The workspace's rules refused the change before it went: the document it writes, the
    /// changes it carries and why. Its author's session is told it, and it is filed where the
    /// person's activity reads it.
    #[error("{0}")]
    ChangeRefused(Box<crate::ChangeRefused>),
    /// The record's stored history was set aside when this device opened the workspace and is
    /// asked of another device again: nothing written to it could be kept until it is back
    /// ([`crate::StatePeer::quarantined`]).
    #[error("{}", crate::NotAdmitted::Restoring)]
    Restoring,
}

impl WriteRefusal {
    /// Why the writes were not taken, as the person is told it.
    #[must_use]
    pub fn reason(&self) -> neighbour_wire::RefusalReason {
        use neighbour_wire::RefusalReason;
        match self {
            Self::HistoryFull { .. } => RefusalReason::Full,
            Self::Waiting(_) | Self::Restoring => RefusalReason::MissingDependency,
            Self::Declined { reason, .. } => *reason,
            Self::ChangeRefused(refused) => refused.reason_class(),
            Self::Unsigned(_) | Self::Refused(_) => RefusalReason::Other,
        }
    }
}

impl From<crate::MembershipError> for WriteRefusal {
    /// A device the workspace grants nothing is told why; a workspace whose rules have not
    /// arrived yet decides once they have, so the writes wait for them.
    fn from(error: crate::MembershipError) -> Self {
        if matches!(error, crate::MembershipError::RulesMissing) {
            return Self::Waiting(crate::admission::MissingPrerequisite::Rules);
        }
        Self::Declined {
            reason: error.refusal_reason(),
            detail: error.to_string(),
        }
    }
}

impl From<WriteRefusal> for String {
    fn from(refusal: WriteRefusal) -> Self {
        refusal.to_string()
    }
}

impl From<String> for WriteRefusal {
    fn from(error: String) -> Self {
        Self::Refused(error)
    }
}

/// This device's writes to one record it has not been sent to a peer yet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocumentWrites {
    pub pending: usize,
    pub durable: u64,
    pub refused: u64,
    /// The latest order a peer kept of the record; none, now that no peer answers.
    pub last_order: Option<u64>,
}

/// An error of a signed turn ([`signed_turn`]): what its turn refused, or its signer's failure.
pub(crate) trait TurnError: From<String> {
    /// The signer did not sign what the turn asked for.
    fn unsigned(error: String) -> Self;
}

impl TurnError for String {
    fn unsigned(error: String) -> Self {
        error
    }
}

impl TurnError for WriteRefusal {
    fn unsigned(error: String) -> Self {
        Self::Unsigned(error)
    }
}

/// How many times one signed turn runs before it gives up: each run signs what the one before it
/// asked, so a turn whose signatures sign over earlier ones (a membership operation on top of the
/// one it just wrote) runs once per link of that chain.
pub(crate) const SIGNED_RUNS: usize = 16;

/// This device's signatures as one turn takes them: what its signer signed for an earlier run of
/// the same turn answers at once; anything else answers with a blank signature, and is kept to be
/// signed outside the turn, so the run still asks for every signature that does not sign over
/// another. A run that asked for one commits nothing ([`Self::settled`]).
pub(crate) struct Signatures {
    device: [u8; 32],
    signed: parking_lot::Mutex<std::collections::BTreeMap<Vec<u8>, [u8; 64]>>,
    wanted: parking_lot::Mutex<std::collections::BTreeSet<Vec<u8>>>,
}

impl Signatures {
    /// The signatures of one turn of `signer`, none given yet.
    pub(crate) fn of(signer: &dyn DeviceSigner) -> Self {
        Self {
            device: signer.device_public_key(),
            signed: parking_lot::Mutex::default(),
            wanted: parking_lot::Mutex::default(),
        }
    }

    /// Have `signer` sign what the last run asked for, for the next run to answer at once.
    /// Answers whether the run asked for any: a run that asked for none is the turn's last.
    pub(crate) async fn sign_wanted(&self, signer: &dyn DeviceSigner) -> Result<bool, String> {
        let wanted = std::mem::take(&mut *self.wanted.lock());
        let asked = !wanted.is_empty();
        for message in wanted {
            let signature = signer
                .sign(message.clone())
                .await
                .map_err(|error| error.to_string())?;
            self.signed.lock().insert(message, signature);
        }
        Ok(asked)
    }

    /// Whether this run signed only with signatures the signer gave: a run that did not commits
    /// nothing, and runs again once they are signed.
    pub(crate) fn settled(&self) -> Result<(), String> {
        if self.wanted.lock().is_empty() {
            Ok(())
        } else {
            Err(UNSIGNED.into())
        }
    }
}

pub(crate) const UNSIGNED: &str = "the device signer signs this change outside the turn";

/// Run `turn`, a change of this device that signs as it goes, with `signer`, this device's
/// ownership of its key: a run that asks for a signature `signer` has not given yet commits
/// nothing ([`Signatures::settled`]), `signer` signs what it asked for outside the turn, and the
/// turn runs again, until one run signed everything it wrote.
///
/// # Errors
/// `turn` refused, signing failed, or the turn still asked for signatures after its last run.
pub(crate) async fn signed_turn<T, E: TurnError>(
    signer: &dyn DeviceSigner,
    mut turn: impl FnMut(&Signatures) -> Result<T, E>,
) -> Result<T, E> {
    let signatures = Signatures::of(signer);
    for _ in 0..SIGNED_RUNS {
        let ran = turn(&signatures);
        if !signatures.sign_wanted(signer).await.map_err(E::unsigned)? {
            return ran;
        }
    }
    Err(E::unsigned(UNSIGNED.into()))
}

impl DeviceSigner for Signatures {
    fn device_public_key(&self) -> [u8; 32] {
        self.device
    }

    fn sign(&self, message: Vec<u8>) -> collab_types::signer::SignFuture<'_> {
        let signed = self.signed.lock().get(&message).copied();
        let signature = signed.unwrap_or_else(|| {
            self.wanted.lock().insert(message);
            [0; 64]
        });
        Box::pin(std::future::ready(Ok(signature)))
    }
}

/// One write this device makes to a record of one of its workspaces: the app, the record, the
/// readers where only some members read it, and the write as this device signed it.
pub struct WorkspaceDocumentWrite {
    pub app: String,
    pub document: String,
    pub readers: Option<String>,
    pub envelope: collab_types::SignedOp,
}

/// The message a write of this device goes out as.
fn message_of(workspace: WorkspaceId, write: &WorkspaceDocumentWrite) -> NeighbourMsg {
    NeighbourMsg::AppDocument {
        peer: crate::peer_handle(write.envelope.author.as_bytes()),
        app_id: write.app.clone(),
        cluster_id: workspace.to_string(),
        document_id: write.document.clone(),
        envelope: Box::new(write.envelope.clone()),
        readers: write.readers.clone(),
    }
}

impl NativeWorkspaceOwner {
    /// Keep this device's verdicts on the document writes `heard` names, each with what
    /// [`Self::receive`] answered for it, in its own slice of `workspace`, signed by `signer`: a
    /// refused write reaches its author, as the next sync carries the verdict back, and a write
    /// that landed clears the verdict it lands on.
    ///
    /// # Errors
    /// The workspace is not held here, `signer` is not this device's, signing failed, or the
    /// verdicts cannot be committed. A verdict the workspace does not take is left out.
    pub async fn keep_verdicts(
        &self,
        workspace: WorkspaceId,
        heard: &[(
            &NeighbourMsg,
            &Result<crate::AdmissionReport, crate::AdmissionError>,
        )],
        signer: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<(), String> {
        let heard = heard
            .iter()
            .filter(|(message, _)| matches!(message, NeighbourMsg::AppDocument { .. }))
            .collect::<Vec<_>>();
        if heard.is_empty() {
            return Ok(());
        }
        signed_turn(signer, |signed| {
            let _turn = self.0.mutation.lock();
            let candidate = self.view(workspace)?;
            let device = candidate.device().clone();
            let (kept, record) = candidate.change(|live, record| -> Result<bool, String> {
                let mut kept = false;
                let mut unkept = Vec::new();
                for (message, report) in &heard {
                    let verdict = match report {
                        // This device opened the sealed change and the workspace's rules refuse
                        // it: its readers read the record without it, and its author is told.
                        Ok(report) if let Some(refused) = report.opened_refusal() => {
                            live.workspace_mut().refuse_write(
                                &device,
                                message,
                                &crate::AdmissionError::ChangeRefused(Box::new(refused.clone())),
                                signed,
                                now,
                            )
                        }
                        Ok(_) => live
                            .workspace_mut()
                            .clear_landed_write(&device, message, signed, now),
                        // This device could not decide the write: no verdict of
                        // this device's says anything about it.
                        Err(error) if error.is_device_failure() => Ok(false),
                        Err(error) => live
                            .workspace_mut()
                            .refuse_write(&device, message, error, signed, now),
                    };
                    match verdict {
                        Ok(filed) => kept |= filed,
                        Err(error) => unkept.push(error),
                    }
                }
                // A record whose checkpoint this device holds the keys of is checked against the
                // writes it stands in for once they are here, and the device keeps that it did:
                // the writes are dropped once every device with the record's keys has.
                let keys = self.0.history_keys.lock().clone();
                if let Some(keys) = keys {
                    let (filed, errors) = check_leading_checkpoints(
                        live,
                        &device,
                        keys.as_ref(),
                        &heard,
                        signed,
                        now,
                        workspace,
                    );
                    kept |= filed;
                    unkept.extend(errors);
                }
                signed.settled()?;
                for error in unkept {
                    tracing::warn!(%error, "a document write's verdict was not kept");
                }
                if kept {
                    record.generation = record.generation.checked_add(1).ok_or_else(|| {
                        WriteRefusal::Refused("native owner generation exhausted".into())
                    })?;
                }
                Ok(kept)
            })?;
            if kept {
                self.publish_document(workspace, &candidate, record, None)?;
            }
            Ok(())
        })
        .await
    }

    /// Admit `writes`, this device's own signed document writes, into `workspace`, keep each as
    /// a frame a link carries to the workspace's other devices, and make them durable: one
    /// record publication for all of them.
    ///
    /// An exact write the record holds already is answered as taken, and a frame it was never
    /// given one for is kept now. Nothing is committed on [`WriteRefusal::HistoryFull`] or a
    /// refusal of the workspace; every write is its own verdict then.
    ///
    /// # Errors
    /// [`WriteRefusal::HistoryFull`], [`WriteRefusal::Waiting`], [`WriteRefusal::Declined`],
    /// [`WriteRefusal::ChangeRefused`], or [`WriteRefusal::Refused`] when the workspace is not
    /// held here or a write is not this device's.
    pub fn admit_writes(
        &self,
        workspace: WorkspaceId,
        writes: Vec<WorkspaceDocumentWrite>,
        now: SystemTime,
    ) -> Result<(), WriteRefusal> {
        if writes.is_empty() {
            return Ok(());
        }
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let device = candidate.device().device_public_key;
        let (changed, mut record) =
            candidate.change(|live, record| -> Result<bool, WriteRefusal> {
                let mut changed = false;
                for write in writes {
                    if write.envelope.author.as_bytes() != &device {
                        return Err(WriteRefusal::Refused(
                            "native local write signer differs from selected device".into(),
                        ));
                    }
                    match live.lookup_exact_local_document(
                        &write.app,
                        &write.document,
                        &write.envelope,
                        now,
                    ) {
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            if !live.sealed_history_takes(
                                &write.app,
                                &write.document,
                                &write.envelope,
                            ) {
                                return Err(WriteRefusal::HistoryFull {
                                    app: write.app.clone(),
                                    record: write.document.clone(),
                                });
                            }
                            live.admit_new_local_document_read_by(
                                (&write.app, &write.document),
                                write.readers.as_deref(),
                                write.envelope.clone(),
                                now,
                            )
                            .map_err(refusal_of)?;
                        }
                        Err(AdmissionError::ChangeRefused(change)) => {
                            return Err(WriteRefusal::ChangeRefused(change));
                        }
                        Err(error) => return Err(WriteRefusal::Refused(error.to_string())),
                    }
                    let bytes = postcard::to_stdvec(&message_of(workspace, &write))
                        .map_err(|error| WriteRefusal::Refused(error.to_string()))?;
                    if !record.frames.iter().any(|frame| frame.bytes == bytes) {
                        record.frames.push(stored::FrameToSend { bytes });
                        changed = true;
                    }
                }
                Ok(changed)
            })?;
        if !changed {
            return Ok(());
        }
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or_else(|| WriteRefusal::Refused("native owner generation exhausted".into()))?;
        self.publish_document(workspace, &candidate, record, None)
            .map_err(WriteRefusal::from)?;
        Ok(())
    }

    /// Draft, seal and sign `write` as `device` by `signer`, admit it into `workspace`, and keep
    /// it as a frame a link carries.
    ///
    /// # Errors
    /// As [`crate::draft::admit_local_write`], and when the record could not be made durable.
    pub async fn write_local(
        &self,
        workspace: WorkspaceId,
        device: [u8; 32],
        signer: Arc<dyn DeviceSigner>,
        write: crate::draft::LocalWrite<'_>,
    ) -> Result<crate::owner::AppWrite, WriteRefusal> {
        let replica = self.replica(workspace).map_err(WriteRefusal::Refused)?;
        let written = crate::draft::admit_local_write(
            replica,
            Some(self.clone()),
            device,
            signer,
            &workspace.to_string(),
            write,
        )
        .await?;
        self.keep_frame(
            workspace,
            &NeighbourMsg::AppDocument {
                peer: written.peer,
                app_id: written.app_id.clone(),
                cluster_id: written.cluster_id.clone(),
                document_id: written.document_id.clone(),
                envelope: Box::new(written.envelope.clone()),
                readers: None,
            },
        )
        .map_err(WriteRefusal::Refused)?;
        Ok(written)
    }

    /// Write `command` into `workspace`'s governance as `caller`, signed by `signer`, and make it
    /// durable.
    ///
    /// # Errors
    /// The workspace is not held here, this device may not make the change, or it could not be
    /// made durable.
    pub async fn commit_membership(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_user_identity::DeviceCert,
        command: &crate::workspace_membership::WorkspaceMembershipCommand,
        signer: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<bool, WriteRefusal> {
        signed_turn(signer, |key| {
            let _turn = self.0.mutation.lock();
            let candidate = self.view(workspace)?;
            let (changed, record) =
                candidate.change(|live, record| -> Result<bool, WriteRefusal> {
                    live.governance_mut()
                        .drop_names_of_nobody(key, now)
                        .map_err(WriteRefusal::from)?;
                    let changed = live
                        .workspace_mut()
                        .apply_workspace_membership_command(caller, command, key, now)
                        .map_err(WriteRefusal::from)?;
                    key.settled()?;
                    if changed {
                        record.generation = record.generation.checked_add(1).ok_or_else(|| {
                            WriteRefusal::Refused("native owner generation exhausted".into())
                        })?;
                    }
                    Ok(changed)
                })?;
            if changed {
                self.publish_document(workspace, &candidate, record, None)
                    .map_err(WriteRefusal::from)?;
            }
            Ok(changed)
        })
        .await
    }

    /// Record `receipt`, the terminal audit of an invocation `caller` served, in its own slice of
    /// `workspace`, as the device's own change, and make it durable.
    ///
    /// # Errors
    /// The workspace is not held here, the receipt is not the caller's, or it could not be made
    /// durable.
    pub fn record_effect(
        &self,
        workspace: WorkspaceId,
        caller: &tilo_user_identity::DeviceCert,
        receipt: &tilo_desktop_model::WorkspaceEffectReceipt,
        now: SystemTime,
    ) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let (changed, mut record) = candidate.change(|live, _record| -> Result<bool, String> {
            live.workspace_mut()
                .apply_own_effect_receipt(caller, receipt, now)
        })?;
        if !changed {
            return Ok(());
        }
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_document(workspace, &candidate, record, None)
    }

    /// Keep `message`, a frame of this device's own, in `workspace`'s record until a link carried
    /// it, and make the record durable.
    ///
    /// # Errors
    /// The workspace is not held here, or the record could not be made durable.
    pub fn keep_frame(&self, workspace: WorkspaceId, message: &NeighbourMsg) -> Result<(), String> {
        let bytes = postcard::to_stdvec(message).map_err(|error| error.to_string())?;
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let mut record = candidate.record.clone();
        if !record.frames.iter().any(|frame| frame.bytes == bytes) {
            record.frames.push(stored::FrameToSend { bytes });
        }
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_document(workspace, &candidate, record, None)
    }

    /// This device's own frames of `workspace` a peer was not sent yet, oldest first,
    /// postcard-encoded.
    #[must_use]
    pub fn frames(&self, workspace: WorkspaceId) -> Vec<Vec<u8>> {
        self.with_current(workspace, |view| {
            Ok(view
                .record
                .frames
                .iter()
                .map(|frame| frame.bytes.clone())
                .collect())
        })
        .unwrap_or_default()
    }

    /// Drop `frame`, one of this device's own, once a link carried it to a peer.
    ///
    /// # Errors
    /// The workspace is not held here, or the record could not be made durable.
    pub fn sent(&self, workspace: WorkspaceId, frame: &[u8]) -> Result<(), String> {
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let mut record = candidate.record.clone();
        let before = record.frames.len();
        record.frames.retain(|kept| kept.bytes != frame);
        if record.frames.len() == before {
            return Ok(());
        }
        record.generation = record
            .generation
            .checked_add(1)
            .ok_or("native owner generation exhausted")?;
        self.publish_document(workspace, &candidate, record, None)
    }

    /// This device's latest compaction of `record` of `app` in `workspace`, as the frame a link
    /// carries, once it is drafted: `None` while none is.
    ///
    /// # Errors
    /// The workspace is not held here.
    pub fn drafted_compaction(
        &self,
        workspace: WorkspaceId,
        (app, record): (&str, &str),
    ) -> Result<Option<Vec<u8>>, String> {
        self.with_current(workspace, |view| {
            Ok(view.record.frames.iter().rev().find_map(|frame| {
                let message = NeighbourMsg::decode(&frame.bytes).ok()?;
                match message {
                    NeighbourMsg::AppDocument {
                        app_id,
                        document_id,
                        envelope,
                        ..
                    } if app_id == app
                        && document_id == record
                        && envelope.kind == collab_types::OpKind::Compaction
                        && !collab_types::is_compaction_segment(&envelope) =>
                    {
                        Some(frame.bytes.clone())
                    }
                    _ => None,
                }
            }))
        })
    }

    /// This device's writes to each record of `workspace` a peer was not sent yet.
    #[must_use]
    pub fn document_writes(
        &self,
        workspace: WorkspaceId,
    ) -> std::collections::BTreeMap<(String, String), DocumentWrites> {
        self.with_current(workspace, |view| {
            let mut writes: std::collections::BTreeMap<(String, String), DocumentWrites> =
                std::collections::BTreeMap::new();
            for frame in &view.record.frames {
                let message = tilo_failure::ignored!(
                    "a frame this device encoded is one it decodes",
                    NeighbourMsg::decode(&frame.bytes).ok()
                );
                if let Some(NeighbourMsg::AppDocument {
                    app_id,
                    document_id,
                    ..
                }) = message
                {
                    let kept = writes.entry((app_id, document_id)).or_default();
                    kept.pending = kept.pending.saturating_add(1);
                }
            }
            Ok(writes)
        })
        .unwrap_or_default()
    }
}

/// What admission refused, as the write path's refusal.
fn refusal_of(refused: crate::NotAdmitted) -> WriteRefusal {
    match refused {
        crate::NotAdmitted::Waiting(prerequisite) => WriteRefusal::Waiting(prerequisite),
        crate::NotAdmitted::HistoryFull => WriteRefusal::HistoryFull {
            app: String::new(),
            record: String::new(),
        },
        crate::NotAdmitted::Restoring => WriteRefusal::Restoring,
        crate::NotAdmitted::Refused { reason, detail } => WriteRefusal::Declined { reason, detail },
        crate::NotAdmitted::ChangeRefused(refused) => WriteRefusal::ChangeRefused(refused),
    }
}

/// Check each record's leading checkpoint in `live` against the writes it stands in for, once
/// they are here, and keep that this device did: the writes are dropped once every device with
/// the record's keys has. Answers what was kept, and why nothing was.
fn check_leading_checkpoints(
    live: &mut crate::StatePeer,
    device: &tilo_user_identity::DeviceCert,
    keys: &dyn super::SealedHistoryKeys,
    heard: &[&(
        &NeighbourMsg,
        &Result<crate::AdmissionReport, crate::AdmissionError>,
    )],
    signed: &dyn DeviceSigner,
    now: SystemTime,
    workspace: WorkspaceId,
) -> (bool, Vec<String>) {
    let mut kept = false;
    let mut unkept = Vec::new();
    let mut records = std::collections::BTreeSet::new();
    for (message, report) in heard {
        let (
            NeighbourMsg::AppDocument {
                app_id,
                document_id,
                ..
            },
            Ok(_),
        ) = (message, report)
        else {
            continue;
        };
        if !records.insert((app_id.as_str(), document_id.as_str())) {
            continue;
        }
        let route = (app_id.as_str(), document_id.as_str());
        let checked = live
            .publication()
            .check_leading_checkpoint(route, &mut |write| keys.open(workspace, write));
        let verdict = match checked {
            Some((checkpoint, true)) => live.workspace_mut().record_checkpoint_check(
                device,
                route,
                &checkpoint,
                signed,
                now,
            ),
            Some((_, false)) => {
                tracing::warn!(
                    app = app_id.as_str(),
                    record = document_id.as_str(),
                    "a checkpoint does not hold what the writes it stands in for \
                     merge to: its readers read the writes instead"
                );
                Ok(false)
            }
            None => Ok(false),
        };
        match verdict {
            Ok(filed) => kept |= filed,
            Err(error) => unkept.push(error),
        }
    }
    (kept, unkept)
}
