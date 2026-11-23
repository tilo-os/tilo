//! This device's writes to its workspaces' records. Every writer (a page, an agent, the desktop,
//! an app's broker) writes the same way: one owner turn drafts and commits the write, and the
//! device's signer signs what the turn asked for between its runs (a page's key signs through a
//! promise), so a run that still asked for a signature commits nothing and runs again.

use crate::AdmissionError;
use crate::owner::{
    NativeCommittedView, NativeWorkspaceOwner, NeighbourMsg, SystemTime, WorkspaceId, answers,
    publication,
};
use crate::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};

/// A write as the owner committed it, with its receipt.
pub type Committed = (OwnerCommitRequest, OwnerCommitReceipt);

/// Why the owner committed none of a device's writes ([`NativeWorkspaceOwner::write`]).
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
    /// changes it carries and why, as a server's refusal of a change names them. Its author's
    /// session is told it, and it is filed where the person's activity reads it.
    #[error("{0}")]
    ChangeRefused(Box<crate::ChangeRefused>),
}

impl WriteRefusal {
    /// Why the writes were not taken, as the person is told it.
    #[must_use]
    pub fn reason(&self) -> neighbour_wire::RefusalReason {
        use neighbour_wire::RefusalReason;
        match self {
            Self::HistoryFull { .. } => RefusalReason::Full,
            Self::Waiting(_) => RefusalReason::MissingDependency,
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

/// This device's signatures as one owner turn takes them: what its signer signed for an earlier
/// run of the same turn answers at once; anything else answers with a blank signature, and is kept
/// to be signed outside the turn, so the run still asks for every signature that does not sign
/// over another. A run that asked for one commits nothing ([`Self::settled`]).
pub(crate) struct Signatures {
    device: [u8; 32],
    signed: parking_lot::Mutex<std::collections::BTreeMap<Vec<u8>, [u8; 64]>>,
    wanted: parking_lot::Mutex<std::collections::BTreeSet<Vec<u8>>>,
}

impl Signatures {
    /// The signatures of one turn of `signer`, none given yet.
    pub(crate) fn of(signer: &dyn collab_types::signer::DeviceSigner) -> Self {
        Self {
            device: signer.device_public_key(),
            signed: parking_lot::Mutex::default(),
            wanted: parking_lot::Mutex::default(),
        }
    }

    /// Have `signer` sign what the last run asked for, for the next run to answer at once.
    /// Answers whether the run asked for any: a run that asked for none is the turn's last.
    pub(crate) async fn sign_wanted(
        &self,
        signer: &dyn collab_types::signer::DeviceSigner,
    ) -> Result<bool, String> {
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

pub(crate) const UNSIGNED: &str = "the device signer signs this change outside the owner's turn";

/// Run `turn`, a change of this device that signs as it goes, with `signer`, this device's
/// ownership of its key: a run that asks for a signature `signer` has not given yet commits
/// nothing ([`Signatures::settled`]), `signer` signs what it asked for outside the owner's
/// turn, and the turn runs again, until one run signed everything it wrote. A run sees what
/// the runs before it saw, or asks again for what changed. A turn whose drafts differ on every
/// run (a sealed write's ciphertext) keeps what its first run drafted.
///
/// # Errors
/// `turn` refused, signing failed, or the turn still asked for signatures after its last run.
pub(crate) async fn signed_turn<T, E: TurnError>(
    signer: &dyn collab_types::signer::DeviceSigner,
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

/// What a signed turn drafts in its first run, with `draft`, which its later runs sign and
/// commit as they are: a draft whose bytes differ every time it is made (a sealed write's
/// ciphertext) is signed once.
pub(crate) fn drafted_once<'a, D>(
    memo: &'a mut Option<D>,
    draft: &mut Option<impl FnOnce(&NativeCommittedView) -> Result<D, String>>,
    view: &NativeCommittedView,
) -> Result<&'a mut D, String> {
    if let Some(draft) = draft.take() {
        *memo = Some(draft(view)?);
    }
    memo.as_mut()
        .ok_or_else(|| "the turn's first run drafted nothing".to_owned())
}

impl collab_types::signer::DeviceSigner for Signatures {
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

impl NativeWorkspaceOwner {
    /// Keep this device's verdicts on the document writes `heard` names, each with what
    /// [`Self::receive`] answered for it, in its own slice of `workspace`, signed by `signer`: a
    /// refused write reaches its author, as the next sync carries the verdict back (a device that
    /// hosts the workspace hands it to the members linked to it as it is kept), and a write that
    /// landed clears the verdict it lands on. One carrier, the workspace document itself.
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
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<(), String> {
        let heard = heard
            .iter()
            .filter(|(message, _)| matches!(message, NeighbourMsg::AppDocument { .. }))
            .collect::<Vec<_>>();
        if heard.is_empty() {
            return Ok(());
        }
        crate::owner::write::signed_turn(signer, |signed| {
            let _turn = self.0.mutation.lock();
            let candidate = self.view(workspace)?;
            let device = candidate.device().clone();
            let (kept, record) = candidate.change(|live, record| {
                let mut kept = false;
                let mut unkept = Vec::new();
                for (message, report) in &heard {
                    let verdict = match report {
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
                signed.settled()?;
                for error in unkept {
                    tracing::warn!(%error, "a document write's verdict was not kept");
                }
                if kept {
                    record.generation = record
                        .generation
                        .checked_add(1)
                        .ok_or("native owner generation exhausted")?;
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

    /// Commit the writes `prepare` drafts from `workspace` as it stands, in order, in one signed
    /// turn of `signer`, this device's ownership of its key: what `prepare` drafted unsigned, and a
    /// write a local writer of this device signed before whose sequence a later write took, is
    /// signed between the turn's runs ([`crate::StatePeer::sequenced_local_document`]). Ownership
    /// takes every one of them, however many wait for the workspace's host already.
    ///
    /// A sealed history is never too long for a write of the device holding its keys: one that
    /// used most of its bound is compacted first ([`Self::compact_outgoing`]), and one the writes
    /// would overflow is compacted then ([`Self::compact_document`]), the writes committed after
    /// the compaction, which ownership sends before them.
    ///
    /// # Errors
    /// [`WriteRefusal::HistoryFull`] when a document's history has no room even compacted,
    /// [`WriteRefusal::Unsigned`] when signing failed, and [`WriteRefusal::Refused`] when the
    /// workspace is not held here, `prepare` refused, or a write is not this device's, not for
    /// `workspace`, or not admitted; nothing is committed then.
    pub async fn write(
        &self,
        workspace: WorkspaceId,
        prepare: impl FnOnce(&NativeCommittedView) -> Result<Vec<OwnerCommitRequest>, String>,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<Vec<Committed>, WriteRefusal> {
        if let Err(error) = self.compact_outgoing(workspace, signer, now).await {
            tracing::warn!(%error, "sealed histories stay uncompacted before a write");
        }
        let mut prepare = Some(prepare);
        let mut drafted = None;
        let mut refused = None;
        let first = {
            let mut turn = |signed: &Signatures| {
                let _turn = self.0.mutation.lock();
                let candidate = self.view(workspace)?;
                let writes = drafted_once(&mut drafted, &mut prepare, &candidate)?;
                self.commit_writes(
                    workspace,
                    &candidate,
                    writes,
                    Some(signed),
                    now,
                    &mut refused,
                )
            };
            signed_turn(signer, &mut turn).await
        };
        let full = match first {
            Err(WriteRefusal::HistoryFull { app, record }) => (app, record),
            written => {
                return self
                    .answered(workspace, written, refused, signer, now)
                    .await;
            }
        };
        // A full record takes no write of anyone's until it is compacted, so it is compacted
        // now even while a compaction that failed before waits out its passes: those are only
        // counted while a history is due, and a history a write overflows before it is due
        // (one large write) would wait for them for good, its writes and every write ownership
        // holds behind them with it.
        let compacted = self
            .compact_document(workspace, (&full.0, &full.1), signer, now)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "a full sealed history stays uncompacted");
                0
            });
        if compacted == 0 {
            let (app, record) = full;
            return Err(WriteRefusal::HistoryFull { app, record });
        }
        // Drafted before the compaction took a sequence of this device: numbered past it again.
        let declined = {
            let mut turn = |signed: &Signatures| {
                let _turn = self.0.mutation.lock();
                let candidate = self.view(workspace)?;
                let writes = drafted_once(&mut drafted, &mut prepare, &candidate)?;
                self.commit_writes(
                    workspace,
                    &candidate,
                    writes,
                    Some(signed),
                    now,
                    &mut refused,
                )
            };
            signed_turn(signer, &mut turn).await
        };
        self.answered(workspace, declined, refused, signer, now)
            .await
    }

    /// What an owner turn came to, a change the workspace's rules refused filed where the person's
    /// activity reads it: its author's session is told the same typed refusal a server's refusal
    /// of a change is, and the workspace's write refusals name the document, the changes it would
    /// have brought and why.
    async fn answered(
        &self,
        workspace: WorkspaceId,
        written: Result<Vec<Committed>, WriteRefusal>,
        refused: Option<NeighbourMsg>,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<Vec<Committed>, WriteRefusal> {
        if let (Err(WriteRefusal::ChangeRefused(change)), Some(message)) = (&written, refused) {
            let heard = Err(crate::AdmissionError::ChangeRefused(change.clone()));
            if let Err(error) = self
                .keep_verdicts(workspace, &[(&message, &heard)], signer, now)
                .await
            {
                tracing::warn!(%error, "a change this device refused was not filed");
            }
        }
        written
    }

    /// [`Self::write`] for one write a local writer of this device signed before the owner's turn.
    ///
    /// # Errors
    /// As [`Self::write`].
    pub async fn commit_local_document(
        &self,
        request: OwnerCommitRequest,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<Committed, WriteRefusal> {
        let workspace = WorkspaceId::parse(&request.workspace)
            .ok_or_else(|| WriteRefusal::Refused("invalid native document workspace".into()))?;
        self.write(workspace, |_| Ok(vec![request]), signer, now)
            .await?
            .pop()
            .ok_or_else(|| WriteRefusal::Refused("native document write committed nothing".into()))
    }

    /// Commit one write this device signed before the owner's turn, with its outgoing ownership.
    ///
    /// # Sequence contract
    /// The envelope is signed before the owner's turn, so its sequence must come from
    /// [`collab_types::next_unordered_document_sequence`]; a writer that needs its write ordered
    /// in the turn writes through [`Self::write`].
    ///
    /// # Errors
    /// As [`Self::write`].
    pub fn commit_document(
        &self,
        request: OwnerCommitRequest,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        let workspace =
            WorkspaceId::parse(&request.workspace).ok_or("invalid native document workspace")?;
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let mut refused = None;
        self.commit_writes(
            workspace,
            &candidate,
            &mut [request],
            None,
            now,
            &mut refused,
        )?
        .pop()
        .map(|(_, receipt)| receipt)
        .ok_or_else(|| "native document write committed nothing".to_owned())
    }

    /// [`Self::commit_document`] for many writes in one turn: one publication for all of them.
    #[cfg(test)]
    pub(crate) fn commit_documents(
        &self,
        mut requests: Vec<OwnerCommitRequest>,
        now: SystemTime,
    ) -> Result<Vec<OwnerCommitReceipt>, String> {
        let Some(first) = requests.first() else {
            return Ok(Vec::new());
        };
        let workspace =
            WorkspaceId::parse(&first.workspace).ok_or("invalid native document workspace")?;
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let mut refused = None;
        Ok(self
            .commit_writes(
                workspace,
                &candidate,
                &mut requests,
                None,
                now,
                &mut refused,
            )?
            .into_iter()
            .map(|(_, receipt)| receipt)
            .collect())
    }

    /// Admit and durably commit writes of this device to `workspace`, in order and in the owner's
    /// turn with their outgoing ownership: one durable publication for all of them. A write the
    /// document holds already is an exact retry, answered as it was taken. A write whose sequence
    /// an earlier write took is numbered past it, and one whose signature does not hold is signed
    /// by `signed`; a run that asked it for a signature commits nothing
    /// ([`Signatures::settled`]). Without `signed`, every write is committed as it was signed.
    /// Nothing is committed on [`WriteRefusal::HistoryFull`] when a document's history has no room
    /// even compacted, [`WriteRefusal::Unsigned`] when signing failed,
    /// [`WriteRefusal::Refused`] when the workspace is not held here, `prepare` refused, or a
    /// write is not this device's, not for `workspace`, or not admitted, and
    /// [`WriteRefusal::ChangeRefused`] when the workspace's rules refuse a change: `refused` then
    /// names the message of the write refused, for the verdict its author's session is told and
    /// the workspace keeps.
    /// Publish `writes`, this device's own document writes that it signed and its replica
    /// admitted already, as its outgoing ownership: what a write made through [`Self::write`]
    /// leaves behind, without drafting, numbering or admitting them a second time. Each op goes
    /// out as it was signed, once; ownership carries it until its host answers.
    ///
    /// # Errors
    /// [`WriteRefusal::HistoryFull`] when a document's history has no room, and
    /// [`WriteRefusal::Refused`] when the workspace is not held here, or a write is not this
    /// device's, not for `workspace`, or not admitted by this replica. Nothing is committed
    /// then.
    pub fn commit_admitted(
        &self,
        workspace: WorkspaceId,
        writes: Vec<OwnerCommitRequest>,
        now: SystemTime,
    ) -> Result<Vec<Committed>, WriteRefusal> {
        if writes.is_empty() {
            return Ok(Vec::new());
        }
        let candidate = self.view(workspace)?;
        let device = candidate.device().device_public_key;
        let runs = self.ownership(workspace);
        let mut full = None;
        let mut waiting: Option<WriteRefusal> = None;
        let changed = {
            let _turn = self.0.mutation.lock();
            candidate.change(|live, stored| {
                let mut taken = Vec::new();
                for request in writes {
                    let CommitOperation::Document {
                        app,
                        document,
                        envelope,
                        ..
                    } = &request.operation
                    else {
                        return Err("unsupported native owner commit family".into());
                    };
                    if WorkspaceId::parse(&request.workspace) != Some(workspace) {
                        return Err("native document write names another workspace".into());
                    }
                    if envelope.author.as_bytes() != &device {
                        return Err(
                            "native local publication signer differs from selected device".into(),
                        );
                    }
                    // The replica must hold this exact op as this device's own write: what the
                    // holder signed and admitted, and nothing else.
                    match live.lookup_exact_local_document(app, document, envelope, now) {
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            return Err("a published write is not admitted by this replica".into());
                        }
                        Err(AdmissionError::ChangeRefused(change)) => {
                            waiting = Some(WriteRefusal::ChangeRefused(change));
                            return Err("the workspace refuses the change".into());
                        }
                        Err(error) => return Err(error.to_string()),
                    }
                    let outgoing = publication::prepare(&request)?;
                    // Published once: a write ownership already carries is not appended again.
                    if publication::has_exact(stored, &outgoing, runs)? {
                        continue;
                    }
                    if !live.sealed_history_takes(app, document, envelope) {
                        full = Some((app.clone(), document.clone()));
                        return Err("sealed record history is full".into());
                    }
                    publication::append_local(stored, outgoing)?;
                    answers::note_unanswered(stored, envelope)?;
                    taken.push(request);
                }
                if taken.is_empty() {
                    return Ok(Vec::new());
                }
                stored.generation = stored
                    .generation
                    .checked_add(1)
                    .ok_or("native owner generation exhausted")?;
                let generation = stored.generation;
                let aggregate_hash = live.durable_hash()?;
                taken
                    .into_iter()
                    .map(|request| {
                        let binding = request.binding()?;
                        Ok((
                            request,
                            OwnerCommitReceipt {
                                binding,
                                generation,
                                aggregate_hash,
                            },
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
        };
        let (committed, stored) = changed.map_err(|error| match (full, waiting) {
            (Some((app, record)), _) => WriteRefusal::HistoryFull { app, record },
            (None, Some(refusal)) => refusal,
            (None, None) => WriteRefusal::Refused(error),
        })?;
        if stored.generation != candidate.generation() {
            // One durable publication for every write taken.
            self.publish_document(workspace, &candidate, stored, None)?;
        }
        Ok(committed)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one owner turn: numbering, admission, the verdict a refused change leaves, and \
                  the publication, read top to bottom"
    )]
    fn commit_writes(
        &self,
        workspace: WorkspaceId,
        candidate: &NativeCommittedView,
        writes: &mut [OwnerCommitRequest],
        signed: Option<&Signatures>,
        now: SystemTime,
        refused: &mut Option<NeighbourMsg>,
    ) -> Result<Vec<Committed>, WriteRefusal> {
        if writes.is_empty() {
            return Ok(Vec::new());
        }
        let device = candidate.device().device_public_key;
        let own = candidate.device().clone();
        if signed.is_some_and(|signed| signed.device != device) {
            return Err(WriteRefusal::Refused(
                "native local publication signer differs from selected device".into(),
            ));
        }
        let runs = self.ownership(workspace);
        let mut full = None;
        let mut waiting: Option<WriteRefusal> = None;
        let changed = candidate.change(|live, stored| {
            let ready = numbered(live, workspace, device, writes, signed, now)?;
            let mut taken = Vec::new();
            let mut fresh = false;
            for (request, checked) in ready {
                let outgoing = publication::prepare(&request)?;
                let CommitOperation::Document {
                    app,
                    document,
                    envelope,
                    readers,
                    ..
                } = &request.operation
                else {
                    return Err("unsupported native owner commit family".into());
                };
                // `checked`: the turn numbered this write itself, so nothing admits it yet.
                let held = if checked {
                    None
                } else {
                    Some(live.lookup_exact_local_document(app, document, envelope, now))
                };
                match held {
                    None | Some(Ok(None)) => {}
                    Some(Ok(Some(_))) => {
                        if !publication::has_exact(stored, &outgoing, runs)?
                            && !publication::retry_covered(live, stored, &outgoing)?
                        {
                            return Err(
                                "exact local admission has no retained publication ownership"
                                    .into(),
                            );
                        }
                        taken.push(request);
                        continue;
                    }
                    // The workspace's rules refuse the change, as deciding it says: its author's
                    // session is told the same typed refusal a server's refusal is.
                    Some(Err(AdmissionError::ChangeRefused(change))) => {
                        *refused = Some(NeighbourMsg::AppDocument {
                            peer: crate::peer_handle(envelope.author.as_bytes()),
                            app_id: app.clone(),
                            cluster_id: workspace.to_string(),
                            document_id: document.clone(),
                            envelope: envelope.clone(),
                            readers: None,
                        });
                        waiting = Some(WriteRefusal::ChangeRefused(change));
                        return Err("the workspace refuses the change".into());
                    }
                    Some(Err(error)) => return Err(error.to_string()),
                }
                if !live.sealed_history_takes(app, document, envelope) {
                    full = Some((app.clone(), document.clone()));
                    return Err("sealed record history is full".into());
                }
                publication::append_local(stored, outgoing)?;
                if let Err(declined) = Self::admit_local_write(
                    live,
                    (app, document),
                    readers.as_deref(),
                    envelope,
                    now,
                ) {
                    if matches!(declined, WriteRefusal::ChangeRefused(_)) {
                        *refused = Some(NeighbourMsg::AppDocument {
                            peer: crate::peer_handle(envelope.author.as_bytes()),
                            app_id: app.clone(),
                            cluster_id: workspace.to_string(),
                            document_id: document.clone(),
                            envelope: envelope.clone(),
                            readers: None,
                        });
                        waiting = Some(declined.clone());
                    } else {
                        waiting =
                            matches!(declined, WriteRefusal::Waiting(_)).then(|| declined.clone());
                    }
                    return Err(declined.to_string());
                }
                // A write of this device the workspace takes says the verdict on it, if there was
                // one, says nothing any more.
                if let Some(signed) = signed {
                    let landed = NeighbourMsg::AppDocument {
                        peer: crate::peer_handle(envelope.author.as_bytes()),
                        app_id: app.clone(),
                        cluster_id: workspace.to_string(),
                        document_id: document.clone(),
                        envelope: envelope.clone(),
                        readers: None,
                    };
                    live.workspace_mut()
                        .clear_landed_write(&own, &landed, signed, now)?;
                }
                answers::note_unanswered(stored, envelope)?;
                taken.push(request);
                fresh = true;
            }
            if fresh {
                stored.generation = stored
                    .generation
                    .checked_add(1)
                    .ok_or("native owner generation exhausted")?;
            }
            let generation = stored.generation;
            let aggregate_hash = live.durable_hash()?;
            let committed = taken
                .into_iter()
                .map(|request| {
                    let binding = request.binding()?;
                    Ok((
                        request,
                        OwnerCommitReceipt {
                            binding,
                            generation,
                            aggregate_hash,
                        },
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(committed)
        });
        let (committed, stored) = changed.map_err(|error| match (full, waiting) {
            (Some((app, record)), _) => WriteRefusal::HistoryFull { app, record },
            (None, Some(refusal)) => refusal,
            (None, None) => WriteRefusal::Refused(error),
        })?;
        if stored.generation != candidate.generation() {
            // One durable publication for every write the turn took.
            self.publish_document(workspace, candidate, stored, None)?;
        }
        Ok(committed)
    }

    /// Admit one local write into the turn's candidate, naming what it waits for when the
    /// workspace has not admitted it here yet.
    fn admit_local_write(
        live: &mut crate::StatePeer,
        route: (&str, &str),
        readers: Option<&str>,
        envelope: &collab_types::SignedOp,
        now: SystemTime,
    ) -> Result<(), WriteRefusal> {
        match live.admit_new_local_document_read_by(route, readers, envelope.clone(), now) {
            Ok(()) => Ok(()),
            Err(crate::NotAdmitted::Waiting(prerequisite)) => {
                Err(WriteRefusal::Waiting(prerequisite))
            }
            Err(crate::NotAdmitted::Refused { reason, detail }) => {
                Err(WriteRefusal::Declined { reason, detail })
            }
            Err(crate::NotAdmitted::HistoryFull) => Err(WriteRefusal::HistoryFull {
                app: route.0.to_owned(),
                record: route.1.to_owned(),
            }),
            Err(crate::NotAdmitted::ChangeRefused(refused)) => {
                Err(WriteRefusal::ChangeRefused(refused))
            }
        }
    }
}

/// `writes` as `workspace`'s record commits them: numbered past what the record holds and what the
/// writes before them took, and signed by `signed`, which asks for every signature this run needs
/// at once. A number is taken once: each write keeps it, so the turn's next run signs the same
/// write. Without `signed`, every write is as it was signed.
/// Whether the document was checked to hold each write already: it does not hold one the turn
/// numbered and signed itself.
fn numbered(
    live: &crate::StatePeer,
    workspace: WorkspaceId,
    device: [u8; 32],
    writes: &mut [OwnerCommitRequest],
    signed: Option<&Signatures>,
    now: SystemTime,
) -> Result<Vec<(OwnerCommitRequest, bool)>, String> {
    let mut taken = std::collections::BTreeMap::new();
    let mut ready = Vec::with_capacity(writes.len());
    for kept in writes.iter_mut() {
        if WorkspaceId::parse(&kept.workspace) != Some(workspace) {
            return Err("native document write names another workspace".into());
        }
        let mut request = kept.clone();
        let CommitOperation::Document {
            app,
            document,
            envelope,
            ..
        } = &mut request.operation
        else {
            return Err("unsupported native owner commit family".into());
        };
        if envelope.author.as_bytes() != &device {
            return Err("native local publication signer differs from selected device".into());
        }
        let mut checked = false;
        if let Some(signed) = signed {
            // A write signed before the turn (an app's, signed by the identity service) names the
            // governance this owner admits it under, and is signed again for it; one the document
            // holds already is a retry, kept as it was taken. A draft this turn made is admitted
            // nowhere: the document is asked nothing of it.
            let retry = collab_types::verify_op(envelope).is_ok()
                && matches!(
                    live.lookup_exact_local_document(app, document, envelope, now),
                    Ok(Some(_))
                );
            if !retry {
                envelope.name_governance(&live.governance_heads()?);
                **envelope = live
                    .sequenced_local_document(app, document, (**envelope).clone(), now)?
                    .0;
                let before = taken.entry((app.clone(), document.clone())).or_insert(0);
                if envelope.seq <= *before {
                    envelope.seq = before.checked_add(1).ok_or("sequence exhausted")?;
                }
                *before = envelope.seq;
                if collab_types::verify_op(envelope).is_err() {
                    collab_types::sign_op(envelope, signed).map_err(|error| error.to_string())?;
                }
                checked = true;
            }
            kept.clone_from(&request);
        }
        ready.push((request, checked));
    }
    signed.map(Signatures::settled).transpose()?;
    Ok(ready)
}
