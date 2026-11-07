//! A key holder compacts an encrypted record's sealed history; a replica
//! without the channel key accepts a verified compaction in place of exactly
//! the writes it covers, and never opens it.
use super::{
    AutoCommit, DocumentMutation, Ed25519PublicKey, NeighbourMsg, OpKind, PeerPublication,
    PeerPublicationMut, Route, SignedOp, SigningKey, StatePeer, SystemTime, err, peer_handle,
};
use collab_types::{ContentRetention, EncryptionMetadata, SealedCompaction};

type Floors = std::collections::HashMap<Ed25519PublicKey, u64>;
/// A key holder sealing a compaction's snapshot under the channel's current
/// epoch, with checkpoint retention.
pub type SealCheckpoint<'a> =
    dyn FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> + 'a;
/// A key holder sealing a write to a record under the channel's current
/// epoch, with delta retention.
pub type SealDelta<'a> = SealCheckpoint<'a>;

/// Writes a sealed history must hold before it is compacted to make workspace in
/// a full exact admission index: fewer would fold little for the work.
pub(super) const PRESSED_HISTORY: usize = 64;

impl PeerPublicationMut<'_> {
    /// Compact `record`'s sealed history as the key holder of its channel
    /// and supersede it here, returning the compaction to publish.
    ///
    /// `open` opens any write of the history, under any epoch whose key is
    /// held, and a previous compaction through
    /// [`collab_types::sealed_ciphertext`]. Every write is opened, decoded
    /// and merged over the record's cleartext base into one document; one
    /// that does not open, decode or merge completely aborts the whole
    /// compaction. `seal` seals that document's snapshot under the channel's
    /// current epoch with [`ContentRetention::Checkpoint`] retention. The
    /// compaction is signed by `key` and admitted here through the same
    /// checks every keyless replica applies, so `key` must be a device the
    /// record admits as a writer of it (or the workspace's governor, compacting
    /// by its `compact` governance action).
    ///
    /// # Errors
    ///
    /// Returns an error when the record holds no sealed history, a write
    /// does not open or merge, the sealed snapshot is not a checkpoint for
    /// the record's channel or does not open to the merged record, or the
    /// compaction is refused by admission; this peer is unchanged then.
    pub fn compact_sealed_history(
        self,
        app: &str,
        record: &str,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
        seal: &mut SealCheckpoint<'_>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let document = self
            .peer
            .core
            .documents
            .get(&route)
            .ok_or("sealed compaction of a record this state does not hold")?;
        if document.sealed.is_empty() {
            return Err("the record holds no sealed history to compact".into());
        }
        let merged = merge_sealed(document, open)?;
        let snapshot = DocumentMutation::Snapshot(merged).encode().map_err(err)?;
        let (metadata, ciphertext) = seal(&snapshot)?;
        if metadata.retention() != Some(ContentRetention::Checkpoint)
            || metadata.channel
                != collab_types::document_channel_id(app, &self.peer.core.workspace, record)
        {
            return Err("a sealed compaction is a checkpoint for the record's channel".into());
        }
        let payload = SealedCompaction::new(&document.sealed.to_vec(), ciphertext)
            .map_err(err)?
            .encode()
            .map_err(err)?;
        let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        let seq = document
            .channel
            .last_seq_per_author
            .get(&author)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("sequence exhausted")?;
        let mut op = SignedOp::new_unsigned_with_encryption(
            payload,
            collab_types::document_artifact_id(app, &self.peer.core.workspace, record),
            author,
            collab_types::document_attestation_id(app, &self.peer.core.workspace, record),
            OpKind::Compaction,
            now,
            seq,
            metadata,
        );
        collab_types::sign_op(&mut op, key).map_err(err)?;
        // Keyless replicas cannot tell an unreadable compaction from a good
        // one, so only one that opens to the merged record leaves here.
        if open(&op)? != snapshot {
            return Err("the sealed compaction does not open to the merged record".into());
        }
        let mut candidate = self.peer.clone();
        candidate.admit_new_local_document(app, record, op.clone(), now)?;
        *self.peer = candidate;
        Ok(NeighbourMsg::AppDocument {
            peer: peer_handle(author.as_bytes()),
            app_id: app.into(),
            cluster_id: self.peer.core.workspace.clone(),
            record_id: record.into(),
            envelope: Box::new(op),
        })
    }
}

/// `document`'s cleartext base with every write of its sealed history opened
/// and merged over it, saved. A replica without the key admits sealed writes
/// in arrival order, not causal order: they merge until nothing more applies.
/// Strict: a compaction replaces the writes it covers, so one that left any
/// of them out would lose it.
pub(super) fn merge_sealed(
    document: &super::Document,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
) -> Result<Vec<u8>, String> {
    // One isolated merge of every write, which takes them in causal order
    // whatever order they were admitted in: a merge per write copied the
    // whole record into the isolate each time.
    let mut history = Vec::new();
    let mut writes = Vec::new();
    for (index, write) in document.sealed.iter().enumerate() {
        let mutation = DocumentMutation::decode(&open(write)?).map_err(err)?;
        match &mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => {
                history.extend_from_slice(bytes);
            }
        }
        writes.push((index, mutation));
    }
    let base = (*document.doc).clone();
    let mut merged = base.clone();
    if !history.is_empty() {
        match crate::document::merge_committed_with_status(
            &mut merged,
            &DocumentMutation::Incremental(history),
        ) {
            Ok(crate::document::MergeOutcome::Changed(_)) => return Ok(merged.save()),
            Ok(crate::document::MergeOutcome::MissingDependencies) => {
                return Err("sealed history does not merge into a complete record".into());
            }
            // One pass is the fast path, and a refused one is answered by the
            // write-by-write merge below.
            // ignored-failure: the merge below reports a refusal that is real.
            Err(refused) => {
                tracing::debug!(error = ?refused, "a sealed history does not merge in one pass");
            }
        }
    }
    let (mut merged, _) = merge_writes(base, writes, LeftOut::ForkOnly)?;
    Ok(merged.save())
}

/// A sealed record as its channel's key holder reads it
/// ([`PeerPublication::opened_document`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedRecord {
    /// The record's cleartext base with every sealed write that merges
    /// merged over it, saved.
    pub document: Vec<u8>,
    /// The writes of the history `document` leaves out, in history order.
    /// They stay in the history: a write they wait for may still arrive,
    /// and the history is never compacted while any is left out.
    pub unmerged: Vec<UnmergedWrite>,
}

impl OpenedRecord {
    /// Whether every write the read leaves out is a repeated writer sequence
    /// ([`UnmergedReason::AlreadyHeld`]): this record keeps the change that
    /// reached it first of such a sequence, and nothing of it is missing.
    #[must_use]
    pub fn only_repeated_sequences(&self) -> bool {
        self.unmerged
            .iter()
            .all(|write| matches!(write.reason, UnmergedReason::AlreadyHeld { .. }))
    }

    /// Whether this record is one compaction makes whole for every reader:
    /// it leaves out writes, and every one of them is a repeated writer
    /// sequence ([`Self::only_repeated_sequences`]).
    #[must_use]
    pub fn needs_repair(&self) -> bool {
        !self.unmerged.is_empty() && self.only_repeated_sequences()
    }
}

/// A sealed write [`OpenedRecord::document`] leaves out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnmergedWrite {
    pub author: [u8; 32],
    pub seq: u64,
    pub reason: UnmergedReason,
}

/// Why a sealed write is left out of a record as its key holder reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UnmergedReason {
    /// It does not open with the keys held, or does not decode.
    Unreadable(String),
    /// Another change of the same writer already holds one of its sequences:
    /// one writer wrote a sequence twice, and the record keeps the change that
    /// reached it first. A replica never adds two changes under one writer's
    /// sequence; nothing it carries is missing from the record.
    AlreadyHeld { actor: String, seq: u64 },
    /// A change it builds on is in neither the record nor its history.
    MissingDependencies {
        /// The first missing change hash of each path back, hex.
        missing: Vec<String>,
    },
}

impl std::fmt::Display for UnmergedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnmergedReason::Unreadable(why) => write!(f, "does not open: {why}"),
            UnmergedReason::AlreadyHeld { actor, seq } => write!(
                f,
                "writes {actor}#{seq} again, which the record holds already"
            ),
            UnmergedReason::MissingDependencies { missing } => write!(
                f,
                "waits for changes the record lacks: {}",
                missing.join(", ")
            ),
        }
    }
}

impl std::fmt::Display for UnmergedWrite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let author = hex::encode(self.author.get(..8).unwrap_or_default());
        write!(f, "{author}#{} {}", self.seq, self.reason)
    }
}

/// What one opened write of a sealed history did when merged over the record
/// as it reads.
pub(super) enum WriteMerge {
    /// Its changes are in the record.
    Applied,
    /// It builds on changes that are not in the record yet.
    Waiting,
    /// Automerge refused it, and another change of the same writer already
    /// holds its sequence: one writer wrote that sequence twice.
    AlreadyHeld { actor: String, seq: u64 },
    /// Automerge refused it for any other reason.
    Refused(String),
}

/// Merge one write of a sealed history, opened and decoded, over `record`.
///
/// A write is decided on its own: a replica adds a change under a writer's
/// sequence once, and a write that writes a sequence the record already holds
/// (a writer that forked, whose two changes share one sequence) is named
/// rather than failing the whole record. The merge keeps the first change of
/// each writer's sequence everywhere a record is read
/// ([`tilo_automerge::keeping`]), so the write's other changes still apply
/// and only the repeated sequence is left out.
pub(super) fn merge_write(record: &mut AutoCommit, mutation: &DocumentMutation) -> WriteMerge {
    match crate::document::merge_committed_with_status(record, mutation) {
        Ok(crate::document::MergeOutcome::Changed(true)) => WriteMerge::Applied,
        Ok(crate::document::MergeOutcome::Changed(false)) => unchanged(record, mutation),
        Ok(crate::document::MergeOutcome::MissingDependencies) => WriteMerge::Waiting,
        Err(crate::document::MergeError::Invalid(why)) => WriteMerge::Refused(why),
        // A write is merged between frames and between actions: it is never
        // handed a record with ops of its own still open.
        Err(crate::document::MergeError::Busy) => {
            WriteMerge::Refused("the record has changes of its own open".into())
        }
    }
}

/// A write that changed nothing: the sequence it writes again when every
/// change of it is one the record already holds, either as itself or as the
/// other change under its writer's sequence.
fn unchanged(record: &mut AutoCommit, mutation: &DocumentMutation) -> WriteMerge {
    let DocumentMutation::Incremental(bytes) = mutation else {
        return WriteMerge::Applied;
    };
    match tilo_automerge::keeping::repeated_sequence(record, bytes) {
        Some(left) => WriteMerge::AlreadyHeld {
            actor: left.actor,
            seq: left.seq,
        },
        None => WriteMerge::Applied,
    }
}

/// What a caller does with a write its record cannot take.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum LeftOut {
    /// Reading a record: leave the write out and name it.
    Named,
    /// Compacting one: leave out only a write whose sequences the record
    /// already holds, and refuse any other. A compaction replaces the writes it
    /// covers, so one that left anything else out would lose it for good.
    ForkOnly,
}

/// Merge the writes of a sealed history, each decided on its own
/// ([`merge_write`]), over `base`; the writes that wait for changes not here yet
/// are tried again once the rest are in.
///
/// Returns the record and every write it leaves out with why, sorted in history
/// order. `policy` decides whether a write left out for any reason other than a
/// fork aborts.
pub(super) fn merge_writes(
    mut merged: AutoCommit,
    writes: Vec<(usize, DocumentMutation)>,
    policy: LeftOut,
) -> Result<(AutoCommit, Vec<(usize, UnmergedReason)>), String> {
    let mut left = Vec::new();
    let mut waiting = writes;
    while !waiting.is_empty() {
        let before = waiting.len();
        let mut next = Vec::with_capacity(before);
        for (index, mutation) in waiting {
            let reason = match merge_write(&mut merged, &mutation) {
                WriteMerge::Applied => continue,
                WriteMerge::Waiting => {
                    next.push((index, mutation));
                    continue;
                }
                WriteMerge::AlreadyHeld { actor, seq } => {
                    UnmergedReason::AlreadyHeld { actor, seq }
                }
                WriteMerge::Refused(why) => UnmergedReason::Unreadable(why),
            };
            if policy == LeftOut::ForkOnly && !matches!(reason, UnmergedReason::AlreadyHeld { .. })
            {
                return Err(format!(
                    "a write of the sealed history does not merge: {reason}"
                ));
            }
            left.push((index, reason));
        }
        if next.len() == before {
            let missing = missing_dependencies(&merged, &next)?;
            for (index, _) in next {
                let reason = UnmergedReason::MissingDependencies {
                    missing: missing.clone(),
                };
                if policy == LeftOut::ForkOnly {
                    return Err(format!(
                        "a write of the sealed history does not merge: {reason}"
                    ));
                }
                left.push((index, reason));
            }
            break;
        }
        waiting = next;
    }
    left.sort_by_key(|(index, _)| *index);
    Ok((merged, left))
}

/// The changes the writes left waiting build on that neither the record nor the
/// writes themselves carry, hex, in the order they are missed.
fn missing_dependencies(
    record: &AutoCommit,
    waiting: &[(usize, DocumentMutation)],
) -> Result<Vec<String>, String> {
    let mut history = Vec::new();
    for (_, mutation) in waiting {
        match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => {
                history.extend_from_slice(bytes);
            }
        }
    }
    // Their changes queue rather than merge, so the record says what they wait
    // for; the scratch copy keeps the queue out of the record.
    let mut scratch = record.clone();
    tilo_automerge_isolate::apply(
        &mut scratch,
        &history,
        tilo_automerge_isolate::Dependencies::RetainOrphans,
    )
    .map_err(err)?;
    Ok(scratch
        .get_missing_deps(&[])
        .iter()
        .map(ToString::to_string)
        .collect())
}

/// [`merge_sealed`] for reading: every write that opens and merges is in the
/// record, and the rest are named instead of refusing the whole record. One
/// write that never arrived (or does not open) must not leave a workspace unable
/// to read its record, and the writes after it are kept, not dropped.
pub(super) fn open_sealed(
    document: &super::Document,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
) -> Result<OpenedRecord, String> {
    let sealed: Vec<&SignedOp> = document.sealed.iter().collect();
    let mut unmerged = Vec::new();
    let mut writes = Vec::new();
    for (index, write) in sealed.iter().enumerate() {
        match open(write).and_then(|plain| DocumentMutation::decode(&plain).map_err(err)) {
            Ok(mutation) => writes.push((index, mutation)),
            Err(why) => unmerged.push((index, UnmergedReason::Unreadable(why))),
        }
    }
    let (mut merged, left) = merge_writes((*document.doc).clone(), writes, LeftOut::Named)?;
    unmerged.extend(left);
    unmerged.sort_by_key(|(index, _)| *index);
    Ok(OpenedRecord {
        document: merged.save_with_options(automerge::SaveOptions {
            retain_orphans: false,
            ..automerge::SaveOptions::default()
        }),
        unmerged: unmerged
            .into_iter()
            .filter_map(|(index, reason)| {
                let write = sealed.get(index)?;
                Some(UnmergedWrite {
                    author: *write.author.as_bytes(),
                    seq: write.seq,
                    reason,
                })
            })
            .collect(),
    })
}

impl PeerPublication<'_> {
    /// `record` of `app` as its channel's key holder reads it: the cleartext
    /// base with every sealed write `open` opens merged over it, and the
    /// writes it leaves out (see [`OpenedRecord`]). A record that is not
    /// encrypted reads as [`StatePeer::document`]. `None` for a record this state
    /// does not hold.
    pub fn opened_document(
        self,
        app: &str,
        record: &str,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    ) -> Result<Option<OpenedRecord>, String> {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let Some(document) = self.peer.core.documents.get(&route) else {
            return Ok(None);
        };
        if !document.encryption_required {
            return Ok(self
                .peer
                .document(app, record)
                .map(|document| OpenedRecord {
                    document,
                    unmerged: Vec::new(),
                }));
        }
        open_sealed(document, open).map(Some)
    }

    /// Whether `record` of `app` is encrypted here: its writes are sealed,
    /// and a write in the clear is refused as a downgrade.
    #[must_use]
    pub fn is_sealed_record(self, app: &str, record: &str) -> bool {
        self.peer.sealed_route(&Route {
            app: app.into(),
            record: record.into(),
        })
    }
}

impl PeerPublicationMut<'_> {
    /// Sign `changes`, Automerge changes to `record` of `app`, as `key`'s
    /// own write, sealed by the channel's key holder with `seal`, and admit
    /// it like any sealed write: it joins the record's sealed history, and
    /// the next durable publication carries it as itself. Members open it
    /// as they open each other's writes. A record held in the clear is
    /// encrypted from it on: the key holder decides that its channel's
    /// writes are sealed.
    ///
    /// # Errors
    ///
    /// Returns an error for a record this state does not hold, when sealing
    /// fails or yields anything but a delta of the record's channel, or when
    /// admission refuses the write; this peer is unchanged then.
    pub fn publish_sealed_incremental(
        self,
        app: &str,
        record: &str,
        changes: Vec<u8>,
        seal: &mut SealDelta<'_>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.publish_sealed(
            app,
            record,
            &DocumentMutation::Incremental(changes),
            seal,
            key,
            now,
        )
    }

    /// Save the whole of `record` of `app` as it stands, sealed by the
    /// channel's key holder with `seal`, signed by `key`, and admit it like
    /// any sealed write. The first write of a record in a workspace whose
    /// channels are sealed is this checkpoint, so the record is sealed from
    /// the start rather than started in the clear and encrypted from its
    /// second write on; every write after it is a sealed change. It founds
    /// the record where this state holds none of it, as the checkpoint a
    /// store keeps at the head of a sealed channel founds it on a device
    /// catching up.
    ///
    /// # Errors
    ///
    /// As [`Self::publish_sealed_incremental`], for a checkpoint of the
    /// record's channel; this peer is unchanged then.
    pub fn publish_sealed_checkpoint(
        self,
        app: &str,
        record: &str,
        document: Vec<u8>,
        seal: &mut SealCheckpoint<'_>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.publish_sealed(
            app,
            record,
            &DocumentMutation::Snapshot(document),
            seal,
            key,
            now,
        )
    }

    fn publish_sealed(
        self,
        app: &str,
        record: &str,
        mutation: &DocumentMutation,
        seal: &mut SealCheckpoint<'_>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        // A change needs the record it changes; a checkpoint is the whole
        // record, so it is all a route it founds needs.
        let retention = match mutation {
            DocumentMutation::Incremental(_) => ContentRetention::Delta,
            DocumentMutation::Snapshot(_) => ContentRetention::Checkpoint,
        };
        let document = self.peer.core.documents.get(&route);
        if document.is_none() && retention == ContentRetention::Delta {
            return Err("sealed write to a record this state does not hold".into());
        }
        // Readers without the key decide a sealed write by the claims of
        // its changes, as a member's page names them: each naming the units
        // its action may write by the surface the op names.
        let surface = self.peer.signed_surface(app);
        let claims = self.peer.surfaces.with_footprints(
            &collab_types::Record {
                room: &self.peer.core.workspace,
                app,
                record,
            },
            document
                .and_then(|held| super::record_kind(&*held.doc))
                .as_deref(),
            surface.as_ref(),
            collab_types::mutation_claims(mutation, collab_types::record_unit).map_err(err)?,
        );
        let plaintext = mutation.encode().map_err(err)?;
        let (metadata, ciphertext) = seal(&plaintext)?;
        if metadata.retention() != Some(retention)
            || metadata.channel
                != collab_types::document_channel_id(app, &self.peer.core.workspace, record)
        {
            return Err("a sealed write is a delta or a checkpoint of the record's channel".into());
        }
        let author = Ed25519PublicKey::from_bytes(key.verifying_key().to_bytes());
        let seq = document
            .and_then(|held| held.channel.last_seq_per_author.get(&author).copied())
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("sequence exhausted")?;
        let mut op = SignedOp::new_unsigned_with_encryption(
            ciphertext,
            collab_types::document_artifact_id(app, &self.peer.core.workspace, record),
            author,
            collab_types::document_attestation_id(app, &self.peer.core.workspace, record),
            OpKind::Change,
            now,
            seq,
            metadata,
        )
        .with_claims(claims)
        .with_surface(surface);
        collab_types::sign_op(&mut op, key).map_err(err)?;
        let mut candidate = self.peer.clone();
        candidate.admit_new_local_document(app, record, op.clone(), now)?;
        *self.peer = candidate;
        Ok(NeighbourMsg::AppDocument {
            peer: peer_handle(author.as_bytes()),
            app_id: app.into(),
            cluster_id: self.peer.core.workspace.clone(),
            record_id: record.into(),
            envelope: Box::new(op),
        })
    }

    /// [`Self::publish_sealed_incremental`] for `changes` a device this
    /// state's authority signs for made, such as a bot it installed
    /// (`delegate`): the workspace decides them as `delegate`'s own sealed write
    /// (its grants over the claims they name, and the name they show), and
    /// the authority's `key` publishes them, as the authority's checkpoint
    /// carries such a device's changes to a record written in the clear.
    ///
    /// # Errors
    ///
    /// As [`Self::publish_sealed_incremental`], and when the workspace would
    /// refuse `delegate`'s own write of `changes`; this peer is unchanged
    /// then.
    pub fn publish_sealed_incremental_for(
        self,
        delegate: &SigningKey,
        (app, record): (&str, &str),
        changes: Vec<u8>,
        seal: &mut SealDelta<'_>,
        key: &SigningKey,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let author = delegate.verifying_key().to_bytes();
        let parsed = collab_types::parse_changes(&changes).map_err(err)?;
        let shows = self.peer.appearance_author(&author, now);
        collab_types::check_appearances(&parsed, shows.as_deref())
            .map_err(|refusal| format!("document change forbidden: {refusal}"))?;
        self.peer
            .clone()
            .publication_mut()
            .publish_sealed_incremental(app, record, changes.clone(), seal, delegate, now)?;
        self.publish_sealed_incremental(app, record, changes, seal, key, now)
    }
}

impl PeerPublication<'_> {
    /// Every encrypted record whose sealed history has used three quarters
    /// of its bound, as `(app, record)`: its key holder compacts it before
    /// new writes are refused.
    #[must_use]
    pub fn sealed_records_to_compact(self) -> Vec<(String, String)> {
        let mut due: Vec<(String, String)> = self
            .peer
            .core
            .documents
            .iter()
            .filter(|(_, document)| {
                document.sealed.len() >= super::MAX_SEALED_OPS / 4 * 3
                    || document.sealed.bytes() >= super::MAX_SEALED_BYTES / 4 * 3
            })
            .map(|(route, _)| (route.app.clone(), route.record.clone()))
            .collect();
        // Every sealed write leaves exact admission evidence until a
        // compaction folds it into its writer's mark. Many records each
        // short of their own bound (the parts of a chained record) fill the
        // workspace's index all the same: once it is half full, the longest
        // history worth folding is due too, one at a time, so the work each
        // write brings stays that of one record.
        if due.is_empty() && self.peer.core.exact_admissions.pressed() {
            due.extend(
                self.peer
                    .core
                    .documents
                    .iter()
                    .filter(|(_, document)| document.sealed.len() >= PRESSED_HISTORY)
                    .max_by_key(|(_, document)| document.sealed.len())
                    .map(|(route, _)| (route.app.clone(), route.record.clone())),
            );
        }
        due
    }
}

impl StatePeer {
    /// The sealed writes the device `author` minted here (its compactions)
    /// that `before` does not hold. A snapshot cannot carry sealed history,
    /// so they are published as themselves.
    pub(super) fn own_sealed_publications(
        &self,
        author: [u8; 32],
        before: Option<&Self>,
    ) -> Vec<NeighbourMsg> {
        let author = Ed25519PublicKey::from_bytes(author);
        let mut messages = Vec::new();
        for (route, document) in &self.core.documents {
            let prior = before.and_then(|before| before.core.documents.get(route));
            // A record still shared with `before` holds nothing new.
            if prior.is_some_and(|prior| super::backend::StoredDocument::ptr_eq(prior, document)) {
                continue;
            }
            let published: Vec<&SignedOp> = prior
                .map(|prior| {
                    prior
                        .sealed
                        .iter()
                        .filter(|op| op.author == author)
                        .collect()
                })
                .unwrap_or_default();
            for op in document.sealed.iter().filter(|op| op.author == author) {
                if !published.contains(&op) {
                    messages.push(NeighbourMsg::AppDocument {
                        peer: peer_handle(author.as_bytes()),
                        app_id: route.app.clone(),
                        cluster_id: self.core.workspace.clone(),
                        record_id: route.record.clone(),
                        envelope: Box::new(op.clone()),
                    });
                }
            }
        }
        messages
    }
}

/// Each author's highest write `compaction` folds into its snapshot.
pub(super) fn covered(compaction: &SignedOp) -> Result<Vec<([u8; 32], u64)>, String> {
    let wrapper = SealedCompaction::parse(compaction).map_err(err)?;
    Ok(wrapper
        .covers
        .iter()
        .map(|(author, sequence)| (*author.as_bytes(), *sequence))
        .collect())
}

/// The history after `compaction`, whose envelope passed every admission
/// check, supersedes `history`: the compaction, then every write it does not
/// cover, in order. `floors` are the record's sequence floors before it.
pub(super) fn supersede(
    history: &[SignedOp],
    floors: &Floors,
    compaction: &SignedOp,
) -> Result<Vec<SignedOp>, String> {
    let wrapper = SealedCompaction::parse(compaction).map_err(err)?;
    if history.is_empty() {
        // Nothing of this route is here to supersede: its key holder's
        // compaction is the whole record, the first thing a replica that
        // holds none of it receives, as a device catching up from a store
        // that keeps only ciphertext does. A replica that holds the route from
        // the governance's coverage holds the floors that says it reached and
        // none of its writes; there is nothing of it to hide from, and the
        // compaction is how the store hands it over.
        return Ok(vec![compaction.clone()]);
    }
    let (superseded, kept): (Vec<&SignedOp>, Vec<&SignedOp>) =
        history.iter().partition(|write| wrapper.covers(write));
    if superseded.is_empty() {
        return Err("sealed compaction supersedes nothing this replica holds".into());
    }
    if covers_beyond(&wrapper, floors) {
        return Err("sealed compaction covers writes this replica has not admitted".into());
    }
    if kept.iter().any(|write| write.kind == OpKind::Compaction) {
        return Err("sealed compaction leaves an earlier compaction in place".into());
    }
    if collab_types::hash_sealed_prefix(superseded.iter().copied()).map_err(err)?
        != wrapper.prefix_hash
    {
        return Err(
            "sealed compaction is partial: it does not cover exactly this replica's writes".into(),
        );
    }
    let epoch = compaction.encryption.map_or(0, |meta| meta.epoch);
    if superseded
        .iter()
        .filter_map(|write| write.encryption)
        .any(|meta| meta.epoch > epoch)
    {
        return Err("sealed compaction is sealed under a stale epoch".into());
    }
    Ok(std::iter::once(compaction).chain(kept).cloned().collect())
}

/// Re-validate a stored history's compaction: only ever its first entry,
/// covering nothing beyond the record's floors and none of the later writes.
pub(super) fn check_stored(history: &[SignedOp], floors: &Floors) -> Result<(), String> {
    for (index, op) in history.iter().enumerate() {
        if op.kind != OpKind::Compaction {
            continue;
        }
        if index != 0 {
            return Err("a sealed compaction is only ever first in a record's history".into());
        }
        let wrapper = SealedCompaction::parse(op).map_err(err)?;
        if covers_beyond(&wrapper, floors) {
            return Err("stored sealed compaction is ahead of the record's watermark".into());
        }
        if history.iter().skip(1).any(|write| wrapper.covers(write)) {
            return Err("stored sealed compaction covers a later write".into());
        }
    }
    Ok(())
}

fn covers_beyond(wrapper: &SealedCompaction, floors: &Floors) -> bool {
    wrapper
        .covers
        .iter()
        .any(|(author, seq)| floors.get(author).is_none_or(|floor| floor < seq))
}
