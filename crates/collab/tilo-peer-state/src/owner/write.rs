//! This device's writes to its workspaces' records. Every writer (a page, an agent, the desktop,
//! an app's broker) writes the same way: the owner drafts the write in one turn, the device's
//! signer signs it outside that turn (a page's key signs through a promise), and a second turn
//! commits it with its outgoing custody. One write of this device runs at a time, from its draft
//! to its commit, so a draft's sequence is still past its record's floor when it commits.

use crate::owner::{
    NativeCommittedView, NativeWorkspaceOwner, NeighbourMsg, SystemTime, WorkspaceId, publication,
};
use crate::owner_commit::{CommitOperation, OwnerCommitReceipt, OwnerCommitRequest};

/// One write [`NativeWorkspaceOwner::write`] commits.
#[derive(Debug)]
pub enum Write {
    /// Drafted in the owner's turn and unsigned: the device's signer signs it.
    Draft(OwnerCommitRequest),
    /// Signed already, as one of this device's local writers signed it.
    Signed(OwnerCommitRequest),
}

/// A write as the owner committed it, with its receipt.
pub type Committed = (OwnerCommitRequest, OwnerCommitReceipt);

/// Sign `envelope`, drafted in an owner's turn, with `signer`, this device's custody of its
/// key, outside that turn.
pub(crate) async fn sign(
    signer: &dyn collab_types::signer::DeviceSigner,
    envelope: &mut collab_types::SignedOp,
) -> Result<(), String> {
    let bytes = envelope
        .signing_bytes()
        .map_err(|error| error.to_string())?;
    let signature = signer
        .sign(bytes)
        .await
        .map_err(|error| error.to_string())?;
    envelope.signature = collab_types::Signature::from_bytes(signature);
    collab_types::verify_op(envelope).map_err(|error| error.to_string())
}

/// How many times one signed turn runs before it gives up: each run signs what the one before it
/// asked, so a turn whose signatures sign over earlier ones (a membership operation on top of the
/// one it just wrote) runs once per link of that chain.
const SIGNED_RUNS: usize = 16;

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

const UNSIGNED: &str = "the device signer signs this change outside the owner's turn";

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
    /// Run `turn`, a change of this device that signs as it goes, with `signer`, this device's
    /// custody of its key: a run that asks for a signature `signer` has not given yet commits
    /// nothing ([`Signatures::settled`]), `signer` signs what it asked for outside the owner's
    /// turn, and the turn runs again, until one run signed everything it wrote. A run sees what
    /// the runs before it saw, or asks again for what changed. `signer` answers at once, as the
    /// custody of every device that holds an owner does ([`collab_types::signer::sign_now`]).
    ///
    /// # Errors
    /// `turn` refused, signing failed, or the turn still asked for signatures after its last run.
    pub(crate) fn signed_turn<T>(
        &self,
        signer: &dyn collab_types::signer::DeviceSigner,
        mut turn: impl FnMut(&Signatures) -> Result<T, String>,
    ) -> Result<T, String> {
        let signatures = Signatures {
            device: signer.device_public_key(),
            signed: parking_lot::Mutex::default(),
            wanted: parking_lot::Mutex::default(),
        };
        for _ in 0..SIGNED_RUNS {
            let ran = turn(&signatures);
            let wanted = std::mem::take(&mut *signatures.wanted.lock());
            if wanted.is_empty() {
                return ran;
            }
            for message in wanted {
                let signature = collab_types::signer::sign_now(signer, message.clone())
                    .map_err(|error| error.to_string())?;
                signatures.signed.lock().insert(message, signature);
            }
        }
        Err(UNSIGNED.into())
    }

    /// Keep this device's verdicts on the document writes `heard` names, each with what
    /// [`Self::receive`] answered for it, in its own slice of `workspace`, signed by `signer`: a
    /// refused write reaches its author, as the next sync carries the verdict back, and a write
    /// that landed clears the verdict it lands on. One carrier, the workspace document itself.
    ///
    /// # Errors
    /// The workspace is not held here, `signer` is not this device's, signing failed, or the
    /// verdicts cannot be committed. A verdict the workspace does not take is left out.
    pub fn keep_verdicts(
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
        self.signed_turn(signer, |signed| {
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
    }

    /// Commit the writes `prepare` drafts from `workspace` as it stands, in order: each draft
    /// signed by `signer`, this device's custody of its key, outside the owner's turn. Commits as
    /// many as the workspace's outgoing custody takes; the rest wait until its host answered some
    /// of what it holds. `whole` commits all of them or, when custody cannot take all, none.
    ///
    /// # Errors
    /// The workspace is not held here, `prepare` refused, signing failed, or a write is not this
    /// device's, not for `workspace`, or not admitted; nothing is committed then.
    pub async fn write(
        &self,
        workspace: WorkspaceId,
        prepare: impl FnOnce(&NativeCommittedView) -> Result<Vec<Write>, String>,
        signer: &dyn collab_types::signer::DeviceSigner,
        whole: bool,
        now: SystemTime,
    ) -> Result<Vec<Committed>, String> {
        let _writing = self.0.writes.lock().await;
        let drafted = {
            let _turn = self.0.mutation.lock();
            prepare(&*self.view(workspace)?)?
        };
        let mut writes = Vec::with_capacity(drafted.len());
        for write in drafted {
            writes.push(match write {
                Write::Signed(request) => request,
                Write::Draft(mut request) => {
                    let CommitOperation::Document { envelope, .. } = &mut request.operation else {
                        return Err("unsupported native owner commit family".into());
                    };
                    sign(signer, envelope).await?;
                    request
                }
            });
        }
        self.commit_writes(workspace, writes, whole, now)
    }

    /// [`Self::write`] for one write a local writer of this device signed before the owner's
    /// turn: drafted again under a sequence past its record's floor when a write another writer
    /// signed later took a greater one ([`crate::StatePeer::sequenced_local_document`]).
    ///
    /// # Errors
    /// As [`Self::write`].
    pub async fn commit_local_document(
        &self,
        request: OwnerCommitRequest,
        signer: &dyn collab_types::signer::DeviceSigner,
        now: SystemTime,
    ) -> Result<Committed, String> {
        let workspace =
            WorkspaceId::parse(&request.workspace).ok_or("invalid native document workspace")?;
        let prepare = |view: &NativeCommittedView| {
            let mut request = request;
            let CommitOperation::Document {
                app,
                record,
                envelope,
            } = &mut request.operation
            else {
                return Err("unsupported native owner commit family".into());
            };
            let (sequenced, unsigned) = view.replica().lock().sequenced_local_document(
                app,
                record,
                (**envelope).clone(),
                now,
            )?;
            **envelope = sequenced;
            Ok(vec![if unsigned {
                Write::Draft(request)
            } else {
                Write::Signed(request)
            }])
        };
        self.write(workspace, prepare, signer, true, now)
            .await?
            .pop()
            .ok_or_else(|| crate::owner::publication::CUSTODY_FULL.to_owned())
    }

    /// Commit one write this device signed before the owner's turn, with its outgoing custody.
    ///
    /// # Sequence contract
    /// The envelope is signed before the owner's turn, so its sequence must come from
    /// [`collab_types::next_unordered_document_sequence`]; a writer that needs its write ordered
    /// in the turn writes through [`Self::write`].
    ///
    /// # Errors
    /// As [`Self::write`], and when custody has no room for it.
    pub fn commit_document(
        &self,
        request: OwnerCommitRequest,
        now: SystemTime,
    ) -> Result<OwnerCommitReceipt, String> {
        let workspace =
            WorkspaceId::parse(&request.workspace).ok_or("invalid native document workspace")?;
        self.commit_writes(workspace, vec![request], true, now)?
            .pop()
            .map(|(_, receipt)| receipt)
            .ok_or_else(|| publication::CUSTODY_FULL.to_owned())
    }

    /// Admit and durably commit signed writes of this device to `workspace`, in order and in one
    /// owner turn with their outgoing custody: one durable publication for all of them. A write
    /// the record holds already is an exact retry, answered as it was taken.
    fn commit_writes(
        &self,
        workspace: WorkspaceId,
        writes: Vec<OwnerCommitRequest>,
        whole: bool,
        now: SystemTime,
    ) -> Result<Vec<Committed>, String> {
        if writes.is_empty() {
            return Ok(Vec::new());
        }
        let _turn = self.0.mutation.lock();
        let candidate = self.view(workspace)?;
        let device = candidate.device().device_public_key;
        let count = writes.len();
        let changed = candidate.change(|live, stored| {
            let mut taken = Vec::new();
            let mut carried = Vec::new();
            let mut fresh = false;
            for request in writes {
                if WorkspaceId::parse(&request.workspace) != Some(workspace) {
                    return Err("native document write names another workspace".into());
                }
                let outgoing = publication::prepare(&request)?;
                let CommitOperation::Document {
                    app,
                    record,
                    envelope,
                } = &request.operation
                else {
                    return Err("unsupported native owner commit family".into());
                };
                if envelope.author.as_bytes() != &device {
                    return Err(
                        "native local publication signer differs from selected device".into(),
                    );
                }
                if live
                    .lookup_exact_local_document(app, record, envelope, now)
                    .map_err(|error| error.to_string())?
                    .is_some()
                {
                    if !stored.hosted_here()
                        && !publication::has_exact(stored, &outgoing)?
                        && !publication::retry_covered(live, stored, &outgoing)?
                    {
                        return Err(
                            "exact local admission has no retained publication custody".into()
                        );
                    }
                    taken.push(request);
                    continue;
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
                match publication::append_local(stored, outgoing) {
                    Err(full) if full == publication::CUSTODY_FULL => break,
                    appended => appended?,
                }
                live.admit_new_local_document(app, record, (**envelope).clone(), now)?;
                taken.push(request);
                fresh = true;
            }
            if whole && taken.len() < count {
                // Leaves the replica as it was.
                return Err(publication::CUSTODY_FULL.into());
            }
            if fresh {
                stored.generation = stored
                    .generation
                    .checked_add(1)
                    .ok_or("native owner generation exhausted")?;
            }
            let generation = stored.generation;
            let aggregate_hash = *blake3::hash(&live.durable()?).as_bytes();
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
        let ((committed, carried), stored) = match changed {
            Err(full) if whole && full == publication::CUSTODY_FULL => return Ok(Vec::new()),
            changed => changed?,
        };
        if stored.generation != candidate.generation() {
            // One durable publication for every write the turn took.
            self.publish_record(workspace, &candidate, stored, None)?;
        }
        for message in carried {
            self.0.kept_here(workspace, &message);
        }
        Ok(committed)
    }
}
