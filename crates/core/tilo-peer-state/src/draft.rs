//! One device's write to one of its workspace's documents: drafted as that device, sealed under
//! the document's channel keys, signed, and admitted into its own replica.
//!
//! Every holder of a device's documents — a workspace service's copy, a page's replica and a
//! session's engine — writes through here, so a write is drafted, sealed and signed in one
//! place whatever holds the replica. What it is sent as afterwards is the signed write itself,
//! never drafted again.

use std::sync::Arc;

use collab_types::signer::DeviceSigner;
use collab_types::{ContentRetention, DocumentMutation, document_artifact_id};
use tilo_action_policy::MemberAccess;
use tilo_desktop_model::WorkspaceId;
use web_time::SystemTime;

use crate::owner::{AppWrite, NativeWorkspaceOwner, WriteRefusal};
use crate::replica::Replica;
use crate::workspace_documents::DocumentWrite;
use crate::{NotAdmitted, SealCheckpoint, StatePeer};

/// A device's write to one document, as it is drafted: `changes` to `app`'s `document`, the
/// whole document when `snapshot` (a founding or a checkpoint), a change of it otherwise, kept
/// for `readers` where only some members read it, sealed under `sealed` (the keys of the
/// document's group) where the workspace seals its documents, as made at `now`.
#[derive(Clone, Copy)]
pub struct LocalWrite<'a> {
    /// The app whose document it is.
    pub app: &'a str,
    /// The document, as its app names it.
    pub document: &'a str,
    /// The reader set of a resource store's document, where it is one.
    pub readers: Option<&'a str>,
    /// The document whole, or only what changed of it.
    pub changes: &'a [u8],
    /// The write founds the document: the workspace must hold nothing of it yet.
    pub founding: bool,
    /// `changes` is the whole document — a founding, or a checkpoint of one the workspace holds
    /// already — rather than a change of it.
    pub snapshot: bool,
    /// The keys that seal it, where the workspace seals its documents.
    pub sealed: Option<&'a key_group::ContentKeys>,
    /// The action it is one part of, when that action wrote more than one record: carried inside
    /// the signed write, so every device takes the action's parts together.
    pub commit: Option<&'a collab_types::WorkspaceCommit>,
    /// When the device makes it.
    pub now: SystemTime,
}

impl LocalWrite<'_> {
    /// The same write, owned: what a write kept for a later turn holds.
    fn owned(self, workspace: &str) -> OwnedWrite {
        OwnedWrite {
            workspace: workspace.to_owned(),
            app: self.app.to_owned(),
            document: self.document.to_owned(),
            readers: self.readers.map(str::to_owned),
            changes: self.changes.to_vec(),
            founding: self.founding,
            snapshot: self.snapshot,
            sealed: self.sealed.cloned(),
            commit: self.commit.cloned(),
            now: self.now,
        }
    }
}

/// [`LocalWrite`], owned: a write handed to [`admit_local_write`], which answers once the device
/// signed and its replica admitted it.
struct OwnedWrite {
    workspace: String,
    app: String,
    document: String,
    readers: Option<String>,
    changes: Vec<u8>,
    founding: bool,
    snapshot: bool,
    sealed: Option<key_group::ContentKeys>,
    commit: Option<collab_types::WorkspaceCommit>,
    now: SystemTime,
}

impl OwnedWrite {
    fn borrowed(&self) -> LocalWrite<'_> {
        LocalWrite {
            app: &self.app,
            document: &self.document,
            readers: self.readers.as_deref(),
            changes: &self.changes,
            founding: self.founding,
            snapshot: self.snapshot,
            sealed: self.sealed.as_ref(),
            commit: self.commit.as_ref(),
            now: self.now,
        }
    }
}

/// The `write` as `device` makes it to `workspace`: the signed-shaped ops to sign and admit, in
/// order, and the author's access in the workspace. A write too large for one frame is cut into
/// segments, each an op of its own, which its readers put back together; every one carries the
/// write's commit. Nothing is signed or admitted here.
///
/// # Errors
///
/// The device may not write it, or a founding write finds the document held already.
pub fn draft_local_write(
    peer: &StatePeer,
    workspace: &str,
    device: &[u8; 32],
    write: LocalWrite<'_>,
) -> Result<(Vec<collab_types::SignedOp>, MemberAccess), WriteRefusal> {
    let LocalWrite {
        app,
        document,
        readers,
        changes,
        founding,
        snapshot,
        sealed,
        commit,
        now,
    } = write;
    if founding && peer.holds_document(app, document) {
        return Err(WriteRefusal::Declined {
            reason: neighbour_wire::RefusalReason::Conflict,
            detail: format!("{document} of {app} is in the workspace already"),
        });
    }
    // A change needs the record it changes: drafted before the record is here, it would be
    // refused and let go, and the next change, which builds on it, would be taken once the record
    // landed: the record would hold a change whose base no device ever sends. It waits for its
    // record as it waits for its keys. A checkpoint is the whole record, so it needs none.
    if !snapshot && !peer.holds_document(app, document) {
        return Err(WriteRefusal::Waiting(
            crate::admission::MissingPrerequisite::CausalDependencies,
        ));
    }
    // Every change names who made it: a change that names no one is refused by every holder, so
    // it is never signed. The device cannot name it here: the name is part of the change's hash,
    // and the writer builds its next change on the hash it made.
    if !snapshot {
        let changes = collab_admission::parse_changes(changes)
            .map_err(|error| WriteRefusal::Refused(error.to_string()))?;
        if let Some(nameless) = changes
            .iter()
            .find(|change| collab_admission::maker_of(change) == collab_admission::Maker::Nobody)
        {
            return Err(WriteRefusal::Refused(format!(
                "a change to {document} of {app} names no one who made it: commit it with its \
                 history entry, or as a framework change such as a native edit ({})",
                nameless.hash()
            )));
        }
    }
    let author = peer.permissions(*device, now)?;
    let artifact = document_artifact_id(app, workspace, document);
    let mutation = if snapshot {
        DocumentMutation::Snapshot(changes.to_vec())
    } else {
        DocumentMutation::Incremental(changes.to_vec())
    };
    // One frame where the write fits one, as every write that fits has always gone.
    let frames = if changes.len() <= collab_types::MAX_DOCUMENT_MUTATION_BYTES {
        mutation.encode().map(|frame| vec![frame])
    } else {
        mutation.encode_frames()
    }
    .map_err(|error| WriteRefusal::Refused(error.to_string()))?;
    let mut seal = sealed.map(|keys| {
        move |plaintext: &[u8]| {
            keys.seal_content(artifact, ContentRetention::of_frame(plaintext), plaintext)
                .map_err(|error| error.to_string())
        }
    });
    let envelopes = frames
        .into_iter()
        .zip(0u64..)
        .map(|(frame, drafted)| {
            let frame = match commit {
                Some(commit) => commit
                    .frame(&frame)
                    .map_err(|error| WriteRefusal::Refused(error.to_string()))?,
                None => frame,
            };
            peer.draft_checked_write(
                (app, document),
                readers,
                &frame,
                seal.as_mut().map(|seal| seal as &mut SealCheckpoint<'_>),
                *device,
                drafted,
                now,
            )
            .map_err(|refusal| match refusal {
                crate::state::DraftRefusal::Change(change) => WriteRefusal::ChangeRefused(change),
                crate::state::DraftRefusal::Refused(reason) => WriteRefusal::Refused(reason),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    // A sealed write split into frames is decided once whole, as the segment that completes it:
    // that one claims the whole write's changes, so a reader that holds the record sealed, and
    // decides it by what its claims name, can decide it.
    let mut envelopes = envelopes;
    if sealed.is_some()
        && envelopes.len() > 1
        && let Some(last) = envelopes.last_mut()
    {
        last.claims = peer.sealed_claims((app, document), &mutation);
    }
    // Taken only as its readers will take it: a write the workspace keeps and its readers
    // refuse would make the whole record unreadable for every one of them. A write split
    // into frames is decided where it is admitted, once it is whole.
    if let [envelope] = envelopes.as_slice() {
        peer.check_drafted_write((app, document), envelope, &mutation)?;
    }
    Ok((envelopes, author))
}

/// Draft `write` against `replica` as it stands, sign it as `device` by `signer`, and admit it
/// into `replica` as that device's own write: the write a device holds, ready to be published as
/// it was signed.
///
/// A sealed record's history has a bound. Where the holder is given the `owner` this device's
/// replica belongs to, it compacts a history that has used most of that bound before the write,
/// and one a write would overflow, then writes: the checkpoint is drafted, sealed, signed and
/// admitted as the key holder's own, and published through the same ownership as the write. A
/// replica held without an owner cannot compact, and answers [`WriteRefusal::HistoryFull`] for
/// its caller to keep the write.
///
/// The write is drafted against the replica as it stands and signed outside its lock: a page's
/// signer answers through a promise, and no link admits in between the draft and the admission.
pub fn admit_local_write(
    replica: Replica,
    owner: Option<NativeWorkspaceOwner>,
    device: [u8; 32],
    signer: Arc<dyn DeviceSigner>,
    workspace: &str,
    write: LocalWrite<'_>,
) -> DocumentWrite {
    let write = write.owned(workspace);
    Box::pin(async move {
        let workspace_id = WorkspaceId::parse(&write.workspace).ok_or_else(|| {
            WriteRefusal::Refused(format!("{} is not a workspace", write.workspace))
        })?;
        // A history that used most of its bound is compacted before every write, as the owner
        // did when the session drafted its writes itself.
        if let Some(owner) = &owner
            && let Err(error) = owner
                .compact_outgoing(workspace_id, signer.as_ref(), write.now)
                .await
        {
            tracing::warn!(%error, "sealed histories stay uncompacted before a write");
        }
        match signed_local_write(&replica, device, signer.as_ref(), &write).await {
            // One write that overfills a record: the record is compacted now, and the write
            // drafted again against what the compaction left.
            Err(full @ WriteRefusal::HistoryFull { .. }) => {
                let Some(owner) = &owner else {
                    return Err(full);
                };
                if let Err(error) = owner
                    .compact_document(
                        workspace_id,
                        (&write.app, &write.document),
                        signer.as_ref(),
                        write.now,
                    )
                    .await
                {
                    tracing::warn!(%error, app = %write.app, record = %write.document,
                        "a full sealed history stays uncompacted");
                }
                signed_local_write(&replica, device, signer.as_ref(), &write).await
            }
            written => written,
        }
    })
}

/// The write `write` signed by `device` by `signer` and admitted into `replica`: the draft, the
/// signature, and the admission, once.
async fn signed_local_write(
    replica: &Replica,
    device: [u8; 32],
    signer: &dyn DeviceSigner,
    write: &OwnedWrite,
) -> Result<AppWrite, WriteRefusal> {
    let drafted = replica.read(StatePeer::clone);
    let (envelopes, author) =
        draft_local_write(&drafted, &write.workspace, &device, write.borrowed())?;
    // The write is drafted against the replica as it stands and signed outside its lock: a page's
    // signer answers through a promise, and no link admits in between the draft and the admission.
    drop(drafted);
    let mut last = None;
    for mut envelope in envelopes {
        let bytes = envelope
            .signing_bytes()
            .map_err(|error| WriteRefusal::Unsigned(error.to_string()))?;
        let signature = signer
            .sign(bytes)
            .await
            .map_err(|error| WriteRefusal::Unsigned(error.to_string()))?;
        envelope.signature = collab_types::Signature(signature);
        replica
            .write(|peer| {
                peer.admit_new_local_document_read_by(
                    (&write.app, &write.document),
                    write.readers.as_deref(),
                    envelope.clone(),
                    write.now,
                )
            })
            .map_err(|refused| match refused {
                NotAdmitted::Waiting(prerequisite) => WriteRefusal::Waiting(prerequisite),
                NotAdmitted::HistoryFull => WriteRefusal::HistoryFull {
                    app: write.app.clone(),
                    record: write.document.clone(),
                },
                NotAdmitted::Restoring => WriteRefusal::Restoring,
                NotAdmitted::Refused { reason, detail } => {
                    WriteRefusal::Declined { reason, detail }
                }
                NotAdmitted::ChangeRefused(change) => WriteRefusal::ChangeRefused(change),
            })?;
        last = Some(envelope);
    }
    let envelope =
        last.ok_or_else(|| WriteRefusal::Refused("a write that carries no frame".into()))?;
    Ok(AppWrite {
        peer: neighbour_wire::peer_handle(&device),
        app_id: write.app.clone(),
        cluster_id: write.workspace.clone(),
        document_id: write.document.clone(),
        author,
        replayed: false,
        envelope,
    })
}
