//! This device's writes to its workspaces' records. Every writer (a page, an agent, the desktop,
//! an app's broker) writes the same way: one owner turn drafts and commits the write, and the
//! device's signer signs what the turn asked for between its runs (a page's key signs through a
//! promise), so a run that still asked for a signature commits nothing and runs again.

use crate::owner::{
    NativeCommittedView, NativeWorkspaceOwner, NeighbourMsg, SystemTime, WorkspaceId, publication,
};
use crate::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};

/// A write as the owner committed it, with its receipt.
pub type Committed = (OwnerCommitRequest, OwnerCommitReceipt);

/// Why the owner committed none of a device's writes ([`NativeWorkspaceOwner::write`]).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WriteRefusal {
    /// The sealed history of `record` of `app` has no room for the writes, and this device could
    /// not compact it: the writes wait until it can.
    #[error("the sealed history of {app}/{record} is full")]
    HistoryFull { app: String, record: String },
    /// This device's signer did not sign the writes.
    #[error("signing failed: {0}")]
    Unsigned(String),
    /// The workspace is not held here, or its record does not take the writes.
    #[error("{0}")]
    Refused(String),
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
/// custody of its key: a run that asks for a signature `signer` has not given yet commits
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
    /// refused write reaches its author, as the next sync carries the verdict back, and a write
    /// that landed clears the verdict it lands on. One carrier, the workspace document itself.
    ///
    /// # Errors
    /// The workspace is not held here, `signer` is not this device's, signing failed, or the
    /// verdicts cannot be committed. A verdict the workspace does not take is left out.
    pub async fn keep_verdicts(
        &self,
        workspace: WorkspaceId,
        heard: &[(&NeighbourMsg, &Result<crate::AdmissionReport, String>)],
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
                        Err(reason) => live
                            .workspace_mut()
                            .refuse_write(&device, message, reason, signed, now),
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
                self.publish_record(workspace, &candidate, record, None)?;
            }
            Ok(())
        })
        .await
    }

    /// Commit the writes `prepare` drafts from `workspace` as it stands, in order, in one signed
    /// turn of `signer`, this device's custody of its key: what `prepare` drafted unsigned, and a
    /// write a local writer of this device signed before whose sequence a later write took, is
    /// signed between the turn's runs ([`crate::StatePeer::sequenced_local_document`]). Custody
    /// takes every one of them, however many wait for the workspace's host already.
    ///
    /// A sealed history is never too long for a write of the device holding its keys: one that
    /// used most of its bound is compacted first ([`Self::compact_outgoing`]), and one the writes
    /// would overflow is compacted then ([`Self::compact_record`]), the writes committed after
    /// the compaction, which custody sends before them.
    ///
    /// # Errors
    /// [`WriteRefusal::HistoryFull`] when a record's history has no room even compacted,
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
        let mut turn = |signed: &Signatures| {
            let _turn = self.0.mutation.lock();
            let candidate = self.view(workspace)?;
            let writes = drafted_once(&mut drafted, &mut prepare, &candidate)?;
            self.commit_writes(workspace, &candidate, writes, Some(signed), now)
        };
        let full = match signed_turn(signer, &mut turn).await {
            Err(WriteRefusal::HistoryFull { app, record }) => (app, record),
            written => return written,
        };
        // A compaction that failed is tried again only after the passes it waits for.
        let waiting = self
            .0
            .compaction_wait
            .lock()
            .get(&workspace)
            .is_some_and(|wait| *wait > 0);
        let compacted = if waiting {
            0
        } else {
            self.compact_record(workspace, (&full.0, &full.1), signer, now)
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(%error, "a full sealed history stays uncompacted");
                    0
                })
        };
        if compacted == 0 {
            let (app, record) = full;
            return Err(WriteRefusal::HistoryFull { app, record });
        }
        // Drafted before the compaction took a sequence of this device: numbered past it again.
        signed_turn(signer, &mut turn).await
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

    /// Commit one write this device signed before the owner's turn, with its outgoing custody.
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
        self.commit_writes(workspace, &candidate, &mut [request], None, now)?
            .pop()
            .map(|(_, receipt)| receipt)
            .ok_or_else(|| "native document write committed nothing".to_owned())
    }

    /// Admit and durably commit writes of this device to `workspace`, in order and in the owner's
    /// turn with their outgoing custody: one durable publication for all of them. A write the
    /// record holds already is an exact retry, answered as it was taken. A write whose sequence an
    /// earlier write took is numbered past it, and one whose signature does not hold is signed by
    /// `signed`; a run that asked it for a signature commits nothing ([`Signatures::settled`]).
    /// Without `signed`, every write is committed as it was signed. A write its record's sealed
    /// history has no room for commits none of them ([`WriteRefusal::HistoryFull`]).
    fn commit_writes(
        &self,
        workspace: WorkspaceId,
        candidate: &NativeCommittedView,
        writes: &mut [OwnerCommitRequest],
        signed: Option<&Signatures>,
        now: SystemTime,
    ) -> Result<Vec<Committed>, WriteRefusal> {
        if writes.is_empty() {
            return Ok(Vec::new());
        }
        let device = candidate.device().device_public_key;
        if signed.is_some_and(|signed| signed.device != device) {
            return Err(WriteRefusal::Refused(
                "native local publication signer differs from selected device".into(),
            ));
        }
        let store = self.0.store.as_ref();
        let mut full = None;
        let changed = candidate.change(|live, stored| {
            let ready = numbered(live, workspace, device, writes, signed, now)?;
            let mut taken = Vec::new();
            let mut carried = Vec::new();
            let mut fresh = false;
            for request in ready {
                let outgoing = publication::prepare(&request)?;
                let CommitOperation::Document {
                    app,
                    record,
                    envelope,
                } = &request.operation
                else {
                    return Err("unsupported native owner commit family".into());
                };
                if live
                    .lookup_exact_local_document(app, record, envelope, now)
                    .map_err(|error| error.to_string())?
                    .is_some()
                {
                    if !stored.hosted_here()
                        && !publication::has_exact(stored, &outgoing, store)?
                        && !publication::retry_covered(live, stored, &outgoing)?
                    {
                        return Err(
                            "exact local admission has no retained publication custody".into()
                        );
                    }
                    taken.push(request);
                    continue;
                }
                if !live.sealed_history_takes(app, record, envelope) {
                    full = Some((app.clone(), record.clone()));
                    return Err("sealed record history is full".into());
                }
                // A workspace this device hosts queues nothing for a host to take: its members
                // linked here hear the commit now, and one linked later catches up on it as the
                // link starts.
                if stored.device == stored.workspace_host {
                    carried.push(
                        postcard::from_bytes::<NeighbourMsg>(&outgoing)
                            .map_err(|error| error.to_string())?,
                    );
                }
                publication::append_local(stored, outgoing)?;
                live.admit_new_local_document(app, record, (**envelope).clone(), now)?;
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
            Ok((committed, carried))
        });
        let ((committed, carried), stored) = changed.map_err(|error| match full {
            Some((app, record)) => WriteRefusal::HistoryFull { app, record },
            None => WriteRefusal::Refused(error),
        })?;
        if stored.generation != candidate.generation() {
            // One durable publication for every write the turn took.
            self.publish_record(workspace, candidate, stored, None)?;
        }
        for message in carried {
            self.0.kept_here(workspace, &message);
        }
        Ok(committed)
    }
}

/// `writes` as `workspace`'s record commits them: numbered past what the record holds and what the
/// writes before them took, and signed by `signed`, which asks for every signature this run needs
/// at once. A number is taken once: each write keeps it, so the turn's next run signs the same
/// write. Without `signed`, every write is as it was signed.
fn numbered(
    live: &crate::StatePeer,
    workspace: WorkspaceId,
    device: [u8; 32],
    writes: &mut [OwnerCommitRequest],
    signed: Option<&Signatures>,
    now: SystemTime,
) -> Result<Vec<OwnerCommitRequest>, String> {
    let mut taken = std::collections::BTreeMap::new();
    let mut ready = Vec::with_capacity(writes.len());
    for kept in writes.iter_mut() {
        if WorkspaceId::parse(&kept.workspace) != Some(workspace) {
            return Err("native document write names another workspace".into());
        }
        let mut request = kept.clone();
        let CommitOperation::Document {
            app,
            record,
            envelope,
        } = &mut request.operation
        else {
            return Err("unsupported native owner commit family".into());
        };
        if envelope.author.as_bytes() != &device {
            return Err("native local publication signer differs from selected device".into());
        }
        if let Some(signed) = signed {
            **envelope = live
                .sequenced_local_document(app, record, (**envelope).clone(), now)?
                .0;
            let before = taken.entry((app.clone(), record.clone())).or_insert(0);
            if envelope.seq <= *before {
                envelope.seq = before.checked_add(1).ok_or("sequence exhausted")?;
            }
            *before = envelope.seq;
            if collab_types::verify_op(envelope).is_err() {
                collab_types::sign_op(envelope, signed).map_err(|error| error.to_string())?;
            }
            kept.clone_from(&request);
        }
        ready.push(request);
    }
    signed.map(Signatures::settled).transpose()?;
    Ok(ready)
}
