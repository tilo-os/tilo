//! A key holder compacts an encrypted document's sealed history; a replica
//! without the document's key accepts a verified compaction in place of exactly
//! the writes it covers, and never opens it.
use super::{
    AutoCommit, DocumentMutation, Ed25519PublicKey, NeighbourMsg, OpKind, PeerPublication,
    PeerPublicationMut, Route, SignedOp, StatePeer, SystemTime, err, peer_handle,
};
use collab_types::signer::DeviceSigner;
use collab_types::{
    ContentRetention, EncryptionMetadata, SealedCompaction, SealedCompactionSegment,
};

type Floors = std::collections::HashMap<Ed25519PublicKey, u64>;
/// A key holder sealing a document's content under its keys' current epoch:
/// a compaction's snapshot with checkpoint retention, a write with delta retention.
pub type SealCheckpoint<'a> =
    dyn FnMut(&[u8]) -> Result<(EncryptionMetadata, Vec<u8>), String> + 'a;

/// The action a sealed write, opened to the plaintext given, waits for the rest of
/// ([`StatePeer::sealed_action_waits`]).
pub(super) type WaitsForAction<'a> =
    dyn Fn(&SignedOp, &[u8]) -> Result<Option<String>, String> + 'a;

/// Writes a sealed history must hold before it is compacted to make workspace in
/// a full exact admission index: fewer would fold little for the work.
pub(super) const PRESSED_HISTORY: usize = 64;

impl<'peer> PeerPublication<'peer> {
    /// Draft the compaction of `record`'s sealed history as the key holder of
    /// its channel, for `author` to sign and
    /// [`PeerPublicationMut::admit_sealed_compaction`] to admit.
    ///
    /// `open` opens any write of the history, under any epoch whose key is
    /// held, and a previous compaction through
    /// [`collab_types::sealed_ciphertext`]. Every write is opened, decoded
    /// and merged over the record's cleartext base into one document; one
    /// that does not open, decode or merge completely aborts the whole
    /// compaction. `rebuilt` replaces that document when the record's key
    /// holder rebuilt it from a history that does not merge whole: one writer
    /// wrote a sequence twice, so no merge holds both changes, and the operator
    /// repairing the record carried what the change left out wrote into it.
    /// Nothing checks that `rebuilt` holds what the writes did: that is the
    /// repair's to answer for. `seal` seals the document's snapshot under the
    /// keys' current epoch with [`ContentRetention::Checkpoint`] retention.
    /// The compaction replaces exactly the writes it covers, and its admission
    /// applies the checks every keyless replica applies, so `author` must be a
    /// device the record admits as a writer of it (or the workspace's host,
    /// compacting by its `compact` governance action).
    ///
    /// # Errors
    ///
    /// The history does not merge, `rebuilt` does not load, or the seal is not
    /// a checkpoint of the record's channel that opens to the document.
    pub fn draft_sealed_compaction(
        self,
        (app, document_id): (&str, &str),
        readers: Option<&str>,
        rebuilt: Option<Vec<u8>>,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
        seal: &mut SealCheckpoint<'_>,
        author: [u8; 32],
        now: SystemTime,
    ) -> Result<Vec<SignedOp>, String> {
        let document = self.document((app, document_id))?;
        // Numbered under the writes it folds: the last of this author's own, 0 when it folds
        // none of them. The device that signs a compaction writes the document as well (a host
        // does what its agents and callers do), numbering each write after its last one of the
        // document, and a reader refuses a write numbered at or below one it took as a replay: a
        // page that opens the document by the fold would refuse every write the author makes
        // next if the fold were numbered past them.
        let seq = document
            .sealed
            .iter()
            .filter(|write| *write.author.as_bytes() == author)
            .map(|write| write.seq)
            .max()
            .unwrap_or(0);
        self.draft(
            ((app, document_id), readers),
            document,
            rebuilt,
            open,
            seal,
            (author, seq),
            now,
        )
    }

    /// Draft the compaction of `history`, a document's history as its server holds it, numbered
    /// as the last of `author`'s writes `history` holds, as [`Self::draft_sealed_compaction`]
    /// drafts the history this replica holds under its own.
    ///
    /// # Errors
    /// As [`Self::draft_sealed_compaction`].
    pub fn draft_sealed_compaction_of(
        self,
        (app, document_id): (&str, &str),
        readers: Option<&str>,
        history: Vec<SignedOp>,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
        seal: &mut SealCheckpoint<'_>,
        (author, seq): ([u8; 32], u64),
        now: SystemTime,
    ) -> Result<Vec<SignedOp>, String> {
        let mut document = self.document((app, document_id))?.clone();
        document.sealed = history.into();
        self.draft(
            ((app, document_id), readers),
            &document,
            None,
            open,
            seal,
            (author, seq),
            now,
        )
    }

    /// The sealed history this replica holds of `record` of `app`, in order.
    #[must_use]
    pub fn sealed_history(self, app: &str, record: &str) -> Vec<SignedOp> {
        self.document((app, record))
            .map(|document| document.sealed.to_vec())
            .unwrap_or_default()
    }

    /// Whether the replica admitted this exact sealed write into the record's history.
    #[must_use]
    pub fn has_sealed_write(self, app: &str, record: &str, envelope: &SignedOp) -> bool {
        self.document((app, record))
            .is_ok_and(|document| document.sealed.iter().any(|held| held == envelope))
    }

    /// Once a document starts using its own group, keep it scoped through
    /// subsequent writes and compaction.
    #[must_use]
    pub fn has_document_keys(self, app: &str, document_id: &str) -> bool {
        self.document((app, document_id)).is_ok_and(|document| {
            document.sealed.iter().any(|op| {
                op.encryption.is_some_and(|metadata| {
                    matches!(
                        metadata.scheme,
                        collab_types::EncryptionScheme::DocumentContentKey { .. }
                    )
                })
            })
        })
    }

    /// The epochs the sealed history this replica holds of `resource` of `app` was sealed under,
    /// ascending: the keys a reader opens it with must hold each of them.
    #[must_use]
    pub fn sealed_epochs(self, app: &str, resource: &str) -> Vec<u64> {
        let epochs: std::collections::BTreeSet<u64> = self
            .document((app, resource))
            .map(|document| {
                document
                    .sealed
                    .iter()
                    .filter_map(|write| write.encryption)
                    .filter(|sealed| {
                        matches!(
                            sealed.scheme,
                            collab_types::EncryptionScheme::ContentKey { .. }
                        )
                    })
                    .map(|sealed| sealed.epoch)
                    .collect()
            })
            .unwrap_or_default();
        epochs.into_iter().collect()
    }

    /// Whether the keys available to a reader cover every sealed write the replica holds.
    #[must_use]
    pub fn sealed_keys_available(
        self,
        app: &str,
        document: &str,
        covers: impl FnMut(collab_types::EncryptionMetadata) -> bool,
    ) -> bool {
        self.document((app, document)).is_ok_and(|held| {
            held.sealed
                .iter()
                .filter_map(|write| write.encryption)
                .all(covers)
        })
    }

    /// Whether the sealed history of `record` of `app` holds `write` as itself.
    #[must_use]
    pub fn holds_sealed(self, app: &str, record: &str, write: &SignedOp) -> bool {
        self.document((app, record)).is_ok_and(|document| {
            document
                .sealed
                .iter()
                .any(|held| held.signature == write.signature)
        })
    }

    /// Check `compaction` as a write of `record` of `app` from its author: signed by it, for
    /// this record, and granted to it.
    ///
    /// A compaction is no change of its own: it carries the writes it folds,
    /// which keep their own invokers, and it is judged by those and by its
    /// signer. It names no invoker of its own and is exempt from that rule,
    /// never from admission.
    ///
    /// # Errors
    /// It is no compaction of this record, or its author may not write it.
    pub fn check_sealed_compaction(
        self,
        (app, record): (&str, &str),
        compaction: &SignedOp,
        now: SystemTime,
    ) -> Result<(), String> {
        if compaction.kind != OpKind::Compaction {
            return Err("a sealed compaction is a compaction write".into());
        }
        SealedCompaction::parse(compaction).map_err(err)?;
        collab_types::validate_whole_document_payload_route(
            app,
            &self.peer.core.workspace,
            record,
            compaction,
        )
        .map_err(err)?;
        collab_types::verify_op(compaction).map_err(err)?;
        match self.peer.authorize_document(
            compaction.author.as_bytes(),
            app,
            record,
            compaction,
            now,
            false,
        )? {
            super::authority::Authorized::Admit { .. } => Ok(()),
            super::authority::Authorized::Hold(crate::MissingPrerequisite::AppSurface) => {
                Err("the compaction's app surface is not here".into())
            }
            super::authority::Authorized::Hold(waiting) => {
                Err(format!("the compaction waits for {waiting}"))
            }
        }
    }

    fn document(
        self,
        (app, record): (&str, &str),
    ) -> Result<&'peer super::backend::StoredDocument, String> {
        self.peer
            .core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .ok_or_else(|| "sealed compaction of a record this state does not hold".into())
    }

    fn draft(
        self,
        ((app, document_id), readers): ((&str, &str), Option<&str>),
        document: &super::Document,
        rebuilt: Option<Vec<u8>>,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
        seal: &mut SealCheckpoint<'_>,
        (author, seq): ([u8; 32], u64),
        now: SystemTime,
    ) -> Result<Vec<SignedOp>, String> {
        let rebuilt = match rebuilt {
            Some(rebuilt) => rebuilt,
            None => merge_sealed_reading(
                document,
                open,
                &mut |write, mutation| {
                    self.peer
                        .check_write_in_log((app, document_id), write, mutation)
                },
                &|checkpoint| self.peer.checkpoint_settled((app, document_id), checkpoint),
            )?,
        };
        AutoCommit::load(&rebuilt).map_err(err)?;
        if document.sealed.is_empty() {
            return Err("the document holds no sealed history to compact".into());
        }
        let snapshot = DocumentMutation::Snapshot(rebuilt);
        // A snapshot larger than one op goes as a segmented transfer: the
        // compaction carries its first segment and names the rest.
        let mut sealed = Vec::new();
        for (index, frame) in snapshot.encode_frames().map_err(err)?.iter().enumerate() {
            let (metadata, ciphertext) = seal(frame)?;
            self.peer.check_document_key_scope(
                (app, document_id),
                readers,
                Some(metadata),
                now,
                false,
            )?;
            let retention = if index == 0 {
                ContentRetention::Checkpoint
            } else {
                ContentRetention::Delta
            };
            if metadata.retention() != Some(retention) {
                return Err("a sealed compaction is a checkpoint for the document".into());
            }
            sealed.push((metadata, ciphertext));
        }
        let mut sealed = sealed.into_iter();
        let (metadata, first) = sealed.next().ok_or("a snapshot encodes to no frame")?;
        let segments = sealed
            .map(|(metadata, ciphertext)| {
                SealedCompactionSegment::new(ciphertext)
                    .encode()
                    .map(|payload| (metadata, payload))
                    .map_err(err)
            })
            .collect::<Result<Vec<_>, String>>()?;
        let payloads: Vec<Vec<u8>> = segments
            .iter()
            .map(|(_, payload)| payload.clone())
            .collect();
        let payload = SealedCompaction::new(&document.sealed.to_vec(), first)
            .map_err(err)?
            .segmented(&payloads)
            .encode()
            .map_err(err)?;
        let op = |payload, metadata| {
            SignedOp::new_unsigned_with_encryption(
                payload,
                collab_types::document_artifact_id(app, &self.peer.core.workspace, document_id),
                Ed25519PublicKey::from_bytes(author),
                collab_types::document_attestation_id(app, &self.peer.core.workspace, document_id),
                OpKind::Compaction,
                now,
                seq,
                metadata,
            )
        };
        let ops: Vec<SignedOp> = std::iter::once(op(payload, metadata))
            .chain(
                segments
                    .into_iter()
                    .map(|(metadata, payload)| op(payload, metadata)),
            )
            .collect();
        // Keyless replicas cannot tell an unreadable compaction from a good
        // one, so only one that opens to the merged document_id leaves here.
        if opened_whole(&ops, open)?.as_ref() != Some(&snapshot) {
            return Err("the sealed compaction does not open to the merged document_id".into());
        }
        Ok(ops)
    }
}

/// What `ops`, a compaction and the segments it names, open to with `open`:
/// `None` while a segment is missing.
fn opened_whole(
    ops: &[SignedOp],
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
) -> Result<Option<DocumentMutation>, String> {
    let mut transfers = crate::document_frame::Transfers::default();
    let mut whole = None;
    for op in ops {
        whole = transfers.take(*op.author.as_bytes(), &open(op)?)?;
    }
    Ok(whole)
}

impl PeerPublicationMut<'_> {
    /// Admit `ops`, a compaction of `record` drafted by
    /// [`PeerPublication::draft_sealed_compaction`] and its segments, each
    /// signed by its author, superseding the history it covers here, and
    /// answer them as the messages to publish, the compaction first. On an
    /// error nothing changes.
    pub fn admit_sealed_compaction(
        self,
        app: &str,
        record: &str,
        readers: Option<&str>,
        ops: Vec<SignedOp>,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut ops = ops.into_iter();
        let op = ops
            .next()
            .ok_or("a sealed compaction is at least one write")?;
        if op.kind != OpKind::Compaction {
            return Err("a sealed compaction is a compaction write".into());
        }
        let segments: Vec<SignedOp> = ops.collect();
        let mut candidate = self.peer.clone();
        for segment in &segments {
            candidate.gather_compaction_piece((app, record), segment.clone())?;
        }
        // The compaction completes what its segments began.
        candidate.admit_new_local_document_read_by((app, record), readers, op.clone(), now)?;
        *self.peer = candidate;
        Ok(std::iter::once(op)
            .chain(segments)
            .map(|op| NeighbourMsg::AppDocument {
                peer: peer_handle(op.author.as_bytes()),
                app_id: app.into(),
                cluster_id: self.peer.core.workspace.clone(),
                readers: readers.map(str::to_owned),
                document_id: record.into(),
                envelope: Box::new(op),
            })
            .collect())
    }

    /// Gather `piece`, a piece of a compaction of `record` of `app` that spans
    /// several ops ([`collab_types::is_compaction_piece`]), and answer the
    /// compaction once all of its pieces are here: [`Self::take_sealed_compaction`]
    /// takes it with them.
    ///
    /// # Errors
    /// It is no piece of a compaction of this record.
    pub fn gather_sealed_piece(
        self,
        (app, record): (&str, &str),
        piece: SignedOp,
    ) -> Result<Option<SignedOp>, String> {
        if !collab_types::is_compaction_piece(&piece) {
            return Err("not a piece of a sealed compaction".into());
        }
        self.peer.gather_compaction_piece((app, record), piece)
    }
}

impl PeerPublicationMut<'_> {
    /// Take `compaction`, which the record's host holds first in its history, as this record's
    /// own history: the compaction, then every write here it does not cover, nor `folded`
    /// picks (the writes an earlier compaction of the server's folded). A compaction of
    /// this device's own that the server does not hold yet goes, and the writes after it stay.
    /// Its caller decides that the compaction holds everything the history it replaces does
    /// (one of this device's own, drafted against what the server acknowledged, or a server's that
    /// covers every write this record folded); it is checked as a write of its author first.
    ///
    /// # Errors
    /// The compaction is no write of its author to this record, or its evidence does not fit
    /// the record's; this peer is unchanged then.
    pub fn take_sealed_compaction(
        self,
        (app, record): (&str, &str),
        compaction: &SignedOp,
        folded: &dyn Fn(&SignedOp) -> bool,
        now: SystemTime,
    ) -> Result<(), String> {
        self.peer
            .publication()
            .check_sealed_compaction((app, record), compaction, now)?;
        let wrapper = SealedCompaction::parse(compaction).map_err(err)?;
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let mut candidate = self.peer.clone();
        candidate
            .publication_mut()
            .record_sealed_compaction(app, record, compaction)?;
        let segments = candidate.compaction_segments(&route, compaction)?;
        let checked = candidate
            .core
            .documents
            .get(&route)
            .and_then(|document| document.sealed.first())
            .filter(|first| super::checkpoints::is_checkpoint(first))
            .is_some_and(|first| candidate.checkpoint_checked_by_all((app, record), first));
        let gathered = !segments.is_empty();
        let workspace = candidate.core.workspace.clone();
        let document = candidate
            .core
            .documents
            .get_mut(&route)
            .ok_or("sealed compaction of a record this state does not hold")?;
        let history: super::backend::SealedLog = std::iter::once(compaction.clone())
            .chain(segments)
            .chain(
                document
                    .sealed
                    .iter()
                    .filter(|write| {
                        write.kind != OpKind::Compaction && !wrapper.covers(write) && !folded(write)
                    })
                    .cloned(),
            )
            .collect::<Vec<_>>()
            .into();
        if !history.within_bounds() {
            return Err("the record's history exceeds its bound".into());
        }
        document.fold_sealed(
            history,
            |op| route.sealed_before_reader_sets(&workspace, op),
            checked,
        );
        let floors = &mut document.channel.last_seq_per_author;
        for (author, seq) in wrapper
            .covers
            .iter()
            .chain([(compaction.author, compaction.seq)].iter())
        {
            let floor = floors.entry(*author).or_default();
            *floor = (*floor).max(*seq);
        }
        if candidate.core.exact_selected {
            candidate
                .core
                .exact_admissions
                .cover(
                    (&candidate.core.workspace, app, record),
                    covered(compaction)?,
                )
                .map_err(err)?;
        }
        if gathered {
            candidate.forget_gathered(&route);
        }
        *self.peer = candidate;
        Ok(())
    }

    /// Make `compaction` the evidence of its author's sequence in `record` of `app`: one this
    /// device drafted again for its host takes the place of the compaction it drafted there
    /// first.
    ///
    /// # Errors
    /// The record's evidence cannot take it.
    pub fn record_sealed_compaction(
        self,
        app: &str,
        record: &str,
        compaction: &SignedOp,
    ) -> Result<(), String> {
        if !self.peer.core.exact_selected {
            return Ok(());
        }
        let core = &mut self.peer.core;
        let admission =
            crate::admission::DocumentAdmission::from_op(&core.workspace, app, record, compaction)
                .map_err(err)?;
        match core.exact_admissions.get(&admission.key) {
            Some(id) if id == &admission.operation_id => return Ok(()),
            Some(_) => core
                .exact_admissions
                .withdraw(&admission.key)
                .map_err(err)?,
            None if core.exact_admissions.covers(&admission.key) => return Ok(()),
            None => {}
        }
        core.exact_admissions.insert(&admission).map_err(err)
    }
}

impl PeerPublicationMut<'_> {
    /// Take `piece`, a compaction of `document` of `app` (or the last piece of one in several
    /// ops) that this replica refuses because it does not hold exactly the writes the
    /// compaction covers: a device that was away, or was ended before it had kept them, while
    /// the history was compacted where the workspace keeps it. From then on those writes are
    /// nowhere but inside the compaction, so a replica that went on refusing it could never
    /// complete the document, and took in nothing written to it afterwards.
    ///
    /// A replica without the keys cannot tell such a compaction from one that leaves writes
    /// out, and refuses both ([`supersede`]). A key holder can tell them apart: `open` opens the
    /// compaction and every write this replica holds that it would replace (the writes it
    /// covers, and a compaction held before it), and the compaction is taken only when it holds
    /// every change of those writes. Nothing this replica holds is given up for it. The history
    /// is then the compaction and the writes it does not cover, as a replica that took the
    /// compaction first holds it. Answers the compaction taken; `None` while pieces of it are
    /// still to come.
    ///
    /// # Errors
    /// It is no compaction of this document its author may write, something does not open, or
    /// the compaction lacks a change this replica holds: the replica is unchanged then.
    pub fn take_compaction_behind(
        self,
        (app, document): (&str, &str),
        piece: SignedOp,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
        now: SystemTime,
    ) -> Result<Option<SignedOp>, String> {
        let mut candidate = self.peer.clone();
        let compaction = if collab_types::is_compaction_piece(&piece) {
            let Some(head) = candidate.gather_compaction_piece((app, document), piece)? else {
                *self.peer = candidate;
                return Ok(None);
            };
            head
        } else {
            piece
        };
        // Its author may write it now, or wrote it before their device was removed and the
        // removal's cut keeps it: a removed device hands nothing over this way.
        candidate
            .publication()
            .check_sealed_compaction((app, document), &compaction, now)?;
        let wrapper = SealedCompaction::parse(&compaction).map_err(err)?;
        if !numbered_under_its_folds(&compaction, &wrapper) {
            return Err(
                "sealed compaction is not numbered as the last write of its author it folds".into(),
            );
        }
        let pieces: Vec<SignedOp> = std::iter::once(compaction.clone())
            .chain(candidate.compaction_segments(
                &Route {
                    app: app.into(),
                    record: document.into(),
                },
                &compaction,
            )?)
            .collect();
        let replaced: Vec<SignedOp> = {
            let history = &candidate.publication().document((app, document))?.sealed;
            if history.is_empty() {
                return Err("the history holds nothing a compaction could be behind".into());
            }
            if history
                .iter()
                .any(|write| write.signature == compaction.signature)
            {
                return Err("the history holds this compaction already".into());
            }
            history
                .iter()
                .filter(|write| write.kind == OpKind::Compaction || wrapper.covers(write))
                .cloned()
                .collect()
        };
        if sealed_under_a_stale_epoch(&compaction, &replaced) {
            return Err("sealed compaction is sealed under a stale epoch".into());
        }
        holds_every_change(
            (&pieces, &replaced),
            candidate.publication().document((app, document))?,
            open,
            &mut |write, mutation| candidate.check_write_in_log((app, document), write, mutation),
            &|checkpoint| candidate.checkpoint_settled((app, document), checkpoint),
        )?;
        candidate.publication_mut().take_sealed_compaction(
            (app, document),
            &compaction,
            &|_| false,
            now,
        )?;
        *self.peer = candidate;
        Ok(Some(compaction))
    }
}

/// Why a key holder refuses a compaction it is behind
/// ([`PeerPublicationMut::take_compaction_behind`]): taking it would give up a change this
/// replica holds.
pub(crate) const LACKS_HELD_WRITES: &str =
    "sealed compaction lacks changes of writes this replica holds";

/// Check that `pieces`, a compaction and the segments it names, hold every change of `replaced`,
/// the writes a replica holds that the compaction would take the place of: merged over what the
/// compaction opens to, they change nothing. A write `check` refuses, and every write built on
/// one, is no change the compaction need hold: every reader leaves it out. The checkpoint
/// `document`, this device's record, leads with is read as every reader reads it
/// ([`super::checkpoints::read_checkpoint`]), `settled` saying whether every device with the
/// record's keys checked it.
///
/// # Errors
/// Something does not open, a write cannot be decided here and now, or a change of `replaced`
/// is not in the compaction.
fn holds_every_change(
    (pieces, replaced): (&[SignedOp], &[SignedOp]),
    document: &super::Document,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    check: &mut CheckOpened<'_>,
    settled: &dyn Fn(&SignedOp) -> bool,
) -> Result<(), String> {
    let Some(DocumentMutation::Snapshot(snapshot)) = opened_whole(pieces, open)? else {
        return Err("the sealed compaction does not open to a whole snapshot".into());
    };
    let mut folded = AutoCommit::new();
    if holds_after(&mut folded, &DocumentMutation::Snapshot(snapshot))?.is_none() {
        return Err("the sealed compaction's snapshot is not complete".into());
    }
    let mut transfers = crate::document_frame::Transfers::default();
    let mut refused = RefusedChanges::of(document);
    let mut writes = Vec::new();
    for (index, write) in replaced.iter().enumerate() {
        let Some(mutation) = transfers.take(*write.author.as_bytes(), &open(write)?)? else {
            continue;
        };
        if write.kind == OpKind::Compaction {
            let read = super::checkpoints::read_checkpoint(
                document,
                &mutation,
                &mut *open,
                &mut *check,
                settled,
            )
            .map_err(|refusal| refusal.to_string())?;
            read.refuse_in(&mut refused);
            writes.push((index, read.instead.unwrap_or(mutation)));
            continue;
        }
        match check(write, &mutation) {
            Ok(()) => writes.push((index, mutation)),
            Err(super::document_capture::OpenedRefusal::Refused(_)) => refused.refuse(&mutation),
            Err(refusal @ super::document_capture::OpenedRefusal::CannotDecide(_)) => {
                return Err(refusal.to_string());
            }
        }
    }
    if transfers.waits_for_changes() {
        return Err("a segmented write of the sealed history is incomplete".into());
    }
    let (_, writes) = refused.left_out(writes);
    let mut changes = Vec::new();
    for (_, mutation) in writes {
        match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => {
                changes.extend_from_slice(&bytes);
            }
        }
    }
    if !changes.is_empty()
        && holds_after(&mut folded, &DocumentMutation::Incremental(changes))? != Some(false)
    {
        return Err(LACKS_HELD_WRITES.into());
    }
    Ok(())
}

/// Merge `mutation` into `merged` and answer whether that changed it: `None` when it builds on
/// changes `merged` does not hold.
fn holds_after(
    merged: &mut AutoCommit,
    mutation: &DocumentMutation,
) -> Result<Option<bool>, String> {
    match crate::document::merge_committed_with_status(merged, mutation) {
        Ok(crate::document::MergeOutcome::Changed(changed)) => Ok(Some(changed)),
        Ok(crate::document::MergeOutcome::MissingDependencies) => Ok(None),
        Err(
            crate::document::MergeError::Invalid(why) | crate::document::MergeError::Device(why),
        ) => Err(why),
        Err(crate::document::MergeError::Busy) => {
            Err("the opened compaction has changes of its own open".into())
        }
    }
}

/// `document`'s cleartext base with every write of its sealed history opened
/// and merged over it, saved. A replica without the key admits sealed writes
/// in arrival order, not causal order: they merge until nothing more applies.
/// Strict: a compaction replaces the writes it covers, so one that left any
/// of them out would lose it. The exception is a write `check` refuses, and
/// every write built on one: every reader leaves those out, so the compaction
/// folds them away for good. A write `check` cannot decide here and now stops
/// the compaction. A checkpoint leading the history is read as every reader reads it, never on
/// its own word ([`super::checkpoints::read_checkpoint`]); none of them counts as checked by every
/// device with the record's keys.
#[cfg(test)]
pub(super) fn merge_sealed(
    document: &super::Document,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    check: &mut CheckOpened<'_>,
) -> Result<Vec<u8>, String> {
    merge_sealed_reading(document, open, check, &|_| false)
}

/// `merge_sealed`, with `settled` saying whether every device with the record's keys checked a
/// checkpoint, which is then read as itself when the writes it stands in for are not here.
pub(super) fn merge_sealed_reading(
    document: &super::Document,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    check: &mut CheckOpened<'_>,
    settled: &dyn Fn(&SignedOp) -> bool,
) -> Result<Vec<u8>, String> {
    // One isolated merge of every write, which takes them in causal order
    // whatever order they were admitted in: a merge per write copied the
    // whole record into the isolate each time.
    let mut writes = Vec::new();
    let mut refused = RefusedChanges::of(document);
    // A write too large for one op, a compaction's snapshot among them, is
    // read once all of its segments are.
    let mut transfers = crate::document_frame::Transfers::default();
    for (index, write) in document.sealed.iter().enumerate() {
        let Some(mutation) = transfers.take(*write.author.as_bytes(), &open(write)?)? else {
            continue;
        };
        // The checkpoint leading the history is read against the writes it stands in for.
        if write.kind == OpKind::Compaction {
            let read = super::checkpoints::read_checkpoint(
                document,
                &mutation,
                &mut *open,
                &mut *check,
                settled,
            )
            .map_err(|refusal| refusal.to_string())?;
            read.refuse_in(&mut refused);
            writes.push((index, read.instead.unwrap_or(mutation)));
            continue;
        }
        // A write this device opened is decided as admission decides the same
        // write in the clear, before the compaction folds it in.
        match check(write, &mutation) {
            Ok(()) => writes.push((index, mutation)),
            Err(super::document_capture::OpenedRefusal::Refused(_)) => refused.refuse(&mutation),
            Err(refusal @ super::document_capture::OpenedRefusal::CannotDecide(_)) => {
                return Err(refusal.to_string());
            }
        }
    }
    // A checkpoint whose segments stopped coming only states again what the writes hold; a
    // compaction leaves it out. Changes still arriving are not left out.
    if transfers.waits_for_changes() {
        return Err("a segmented write of the sealed history is incomplete".into());
    }
    let (_, writes) = refused.left_out(writes);
    let mut history = Vec::new();
    for (_, mutation) in &writes {
        match mutation {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => {
                history.extend_from_slice(bytes);
            }
        }
    }
    let base = (*document.doc).clone();
    let mut merged = base.clone();
    // A record with rules of its own reads each write by them, which one pass over the whole
    // history does not.
    if !history.is_empty() && !refused.ruled() {
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
            Err(refused) => {
                tilo_failure::ignored!(
                    "the merge below reports a refusal that is real",
                    tracing::debug!(error = ?refused, "a sealed history does not merge in one pass")
                );
            }
        }
    }
    let (mut merged, _) = merge_writes(base, writes, LeftOut::ForkOnly, None, &mut refused)?;
    Ok(merged.save())
}

/// A sealed record as its channel's key holder reads it
/// ([`PeerPublication::opened_document`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedDocument {
    /// The record's cleartext base with every sealed write that merges
    /// merged over it, saved.
    pub document: Vec<u8>,
    /// The writes of the history `document` leaves out, in history order.
    /// They stay in the history: a write they wait for may still arrive,
    /// and the history is never compacted while any is left out.
    pub unmerged: Vec<UnmergedWrite>,
}

/// A sealed write [`OpenedDocument::document`] leaves out.
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
    /// It builds on a change its own writer never sent, and that no write still on its way can
    /// carry: its writer lost that change before signing it, and every write its writer signed
    /// before this one is held. It is that writer's follow-up to a lost change, and is lost too.
    BehindUnsent {
        /// The first missing change hash of each path back, hex.
        missing: Vec<String>,
    },
    /// The workspace's rules refuse it, as they refuse the same write sent in the clear: what it
    /// changes, or who it says asked for it, is not its author's to write. Every reader that opens
    /// it reaches the same verdict, and reads the record without it.
    Refused {
        /// Why, in the refusing device's words.
        refusal: String,
    },
    /// It builds on a write the workspace's rules refuse, directly or through another write that
    /// does: only that write's author ever took the refused changes in, so these are its own
    /// follow-ups, read without along with it.
    BuiltOnRefused {
        /// The refused changes it builds on, hex.
        on: Vec<String>,
    },
    /// It, or a write before it, is one write of an action whose writes to other records are not
    /// all here yet: the records change together once they are.
    WaitsForAction {
        /// The action, named.
        action: String,
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
            UnmergedReason::BehindUnsent { missing } => write!(
                f,
                "builds on a change its writer never sent: {}",
                missing.join(", ")
            ),
            UnmergedReason::Refused { refusal } => write!(f, "is refused: {refusal}"),
            UnmergedReason::BuiltOnRefused { on } => {
                write!(f, "builds on a refused change: {}", on.join(", "))
            }
            UnmergedReason::WaitsForAction { action } => {
                write!(f, "waits for the rest of {action}")
            }
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
    /// This device could not decide it (an isolated worker crashed or overran
    /// its deadline). Nothing is known about the write itself.
    Device(String),
}

/// Merge one write of a sealed history, opened and decoded, over `record`.
///
/// A write is decided on its own: a replica adds a change under a writer's
/// sequence once, and a write that writes a sequence the record already holds
/// (a writer that forked, whose two changes share one sequence) is named
/// rather than failing the whole record. Automerge itself refuses the second
/// change under a sequence, so that refusal is read as the fork it is.
pub(super) fn merge_write(record: &mut AutoCommit, mutation: &DocumentMutation) -> WriteMerge {
    match merge_committed(record, mutation) {
        Ok(crate::document::MergeOutcome::Changed(_)) => WriteMerge::Applied,
        Ok(crate::document::MergeOutcome::MissingDependencies) => WriteMerge::Waiting,
        Err(crate::document::MergeError::Invalid(why)) => match already_held(record, mutation) {
            Some((actor, seq)) => WriteMerge::AlreadyHeld { actor, seq },
            None => WriteMerge::Refused(why),
        },
        Err(crate::document::MergeError::Device(reason)) => WriteMerge::Device(reason),
        // A write is merged between frames and between actions: it is never
        // handed a record with ops of its own still open.
        Err(crate::document::MergeError::Busy) => {
            WriteMerge::Refused("the record has changes of its own open".into())
        }
    }
}

/// Merge `mutation`, a write of a sealed history, into `record`: a whole document (a compaction,
/// which folds the record's base and the writes before it) is loaded on its own, as into a blank
/// record, and what `record` holds beyond it merged into it. Merged into `record` the other way,
/// each of the document's changes was applied to the record one at a time, in the isolate and
/// again here: a compacted part of a tracker opened no faster than the writes it folds replayed.
fn merge_committed(
    record: &mut AutoCommit,
    mutation: &DocumentMutation,
) -> Result<crate::document::MergeOutcome, crate::document::MergeError> {
    if let DocumentMutation::Snapshot(bytes) = mutation
        && automerge::transaction::Transactable::pending_ops(record) == 0
        && !record.get_heads().is_empty()
        && record.get_missing_deps(&[]).is_empty()
        && let Ok(mut loaded) = tilo_automerge_isolate::load(bytes)
        && tilo_automerge::ApplyIncoming::merge_incoming(&mut loaded, record).is_ok()
    {
        let changed = loaded.get_heads() != record.get_heads();
        let actor = record.get_actor().clone();
        *record = loaded.with_actor(actor);
        return Ok(crate::document::MergeOutcome::Changed(changed));
    }
    crate::document::merge_committed_with_status(record, mutation)
}

/// The change of `mutation` whose writer and sequence `record` already holds,
/// when it carries every change of that write: `None` when any of its changes
/// would be new (then the write is not merely a second one).
fn already_held(record: &mut AutoCommit, mutation: &DocumentMutation) -> Option<(String, u64)> {
    let DocumentMutation::Incremental(bytes) = mutation else {
        return None;
    };
    let changes = tilo_failure::ignored!(
        "change bytes that do not parse are no second write of a sequence",
        tilo_automerge_isolate::parse_changes(bytes).ok()
    )?;
    if changes.is_empty() {
        return None;
    }
    let held = record.get_changes(&[]);
    let mut found = None;
    for change in &changes {
        let is_held = held
            .iter()
            .any(|held| held.actor_id() == change.actor_id() && held.seq() == change.seq());
        if !is_held {
            return None;
        }
        if found.is_none() {
            found = Some((change.actor_id().to_hex_string(), change.seq()));
        }
    }
    found
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

/// A write of a sealed history a merge leaves out: where it is in the history, why, and what it
/// writes.
pub(super) type LeftWrite = (usize, UnmergedReason, DocumentMutation);

/// The changes of the writes of a history that the workspace's rules refuse, and of every write
/// built on one of them: a reader leaves each such write out, and the record reads without them.
/// The verdict on a write is the same on every device that opens it, so every reader leaves out
/// the same writes, and a compaction folds them away.
#[derive(Clone, Debug, Default)]
pub(super) struct RefusedChanges(
    std::collections::HashSet<automerge::ChangeHash>,
    Option<ContentRule>,
);

/// What a record's own rules say of one of its changes, read from the record that holds it and
/// every change it builds on: why it is refused. A rule that a reader of the record decides from
/// what the change wrote, which a holder without its keys cannot.
pub(super) type ContentRule = fn(&AutoCommit, &automerge::Change) -> Result<(), String>;

impl RefusedChanges {
    /// The changes refused in reading `document`, decided by its own rules as well as the
    /// workspace's: a workspace's shared desktop refuses a change that renames a strip without
    /// claiming it ([`tilo_desktop_model::shared_resource::guard::change_renames_claimed`]),
    /// which the claim alone, all a holder without its keys decides by, cannot show.
    pub(super) fn of(document: &super::Document) -> Self {
        use tilo_desktop_model::ownership::{DESKTOP_APP, DESKTOP_DOCUMENT};
        let desktop = collab_types::document_artifact_id(
            DESKTOP_APP,
            &document.channel.workspace,
            DESKTOP_DOCUMENT,
        );
        let is_desktop = document.channel.artifacts.contains_key(&desktop)
            || document.sealed.iter().any(|op| op.artifact == desktop);
        let rule: Option<ContentRule> = is_desktop.then_some(|doc, change| {
            tilo_desktop_model::shared_resource::guard::change_renames_claimed(doc, change)
                .map_err(|refusal| refusal.to_string())
        });
        Self(std::collections::HashSet::new(), rule)
    }

    /// Whether the record's own rules read its writes ([`Self::of`]).
    pub(super) const fn ruled(&self) -> bool {
        self.1.is_some()
    }

    /// Leave out `mutation`, a write the workspace's rules refuse: what is built on its changes is
    /// left out with it. A whole document names no change of its own apart from those it folds,
    /// which other writes build on; it is left out alone.
    pub(super) fn refuse(&mut self, mutation: &DocumentMutation) {
        if let DocumentMutation::Incremental(bytes) = mutation
            && let Some(hashes) = tilo_automerge_isolate::change_hashes(bytes)
        {
            self.0.extend(hashes);
        }
    }

    /// Leave out `hashes`, changes the workspace's rules refuse, found where another reading
    /// decided them: what is built on them is left out with them.
    pub(super) fn extend(&mut self, hashes: impl IntoIterator<Item = automerge::ChangeHash>) {
        self.0.extend(hashes);
    }

    /// The changes left out, in no order.
    pub(super) fn hashes(&self) -> Vec<automerge::ChangeHash> {
        self.0.iter().copied().collect()
    }

    /// `writes` parted into those built on a refused change ([`UnmergedReason::BuiltOnRefused`]),
    /// whose changes are refused from then on too, and the rest, each in `writes`' order.
    pub(super) fn left_out(
        &mut self,
        writes: Vec<(usize, DocumentMutation)>,
    ) -> (Vec<LeftWrite>, Vec<(usize, DocumentMutation)>) {
        if self.0.is_empty() || writes.is_empty() {
            return (Vec::new(), writes);
        }
        let before = self.0.clone();
        let mutations: Vec<&DocumentMutation> =
            writes.iter().map(|(_, mutation)| mutation).collect();
        let built = super::unsent_bases::built_on(&mut self.0, &mutations);
        let mut left = Vec::new();
        let mut kept = Vec::new();
        for ((index, mutation), built) in writes.into_iter().zip(built) {
            if built {
                let on = refused_bases(&before, &self.0, &mutation);
                left.push((index, UnmergedReason::BuiltOnRefused { on }, mutation));
            } else {
                kept.push((index, mutation));
            }
        }
        (left, kept)
    }
}

/// The refused changes `mutation` builds on directly, hex: those of `before`, the writes refused
/// to begin with, where it builds on one of them, else any of `refused`.
fn refused_bases(
    before: &std::collections::HashSet<automerge::ChangeHash>,
    refused: &std::collections::HashSet<automerge::ChangeHash>,
    mutation: &DocumentMutation,
) -> Vec<String> {
    let DocumentMutation::Incremental(bytes) = mutation else {
        return Vec::new();
    };
    let changes = tilo_failure::ignored!(
        "a write whose changes do not parse names no base",
        tilo_automerge_isolate::parse_changes(bytes).unwrap_or_default()
    );
    let own: std::collections::HashSet<automerge::ChangeHash> =
        changes.iter().map(automerge::Change::hash).collect();
    let deps = || {
        changes
            .iter()
            .flat_map(|change| change.deps().iter().copied())
            .filter(|dep| !own.contains(dep))
    };
    let mut on: Vec<String> = deps()
        .filter(|dep| before.contains(dep))
        .map(|dep| dep.to_string())
        .collect();
    if on.is_empty() {
        on = deps()
            .filter(|dep| refused.contains(dep))
            .map(|dep| dep.to_string())
            .collect();
    }
    on.sort();
    on.dedup();
    on
}

/// Who signed each write of a history, by its place in it, and how far each signer's sequence is
/// held: what tells a write stuck behind a change its writer never sent
/// ([`super::unsent_bases`]) from one whose base is still on its way.
pub(super) struct Signers<'a> {
    pub(super) of: &'a dyn Fn(usize) -> Option<super::unsent_bases::Signed>,
    pub(super) held: &'a dyn Fn() -> super::unsent_bases::HeldThrough,
}

/// Merge the writes of a sealed history, each decided on its own
/// ([`merge_write`]), over `base`; the writes that wait for changes not here yet
/// are tried again once the rest are in.
///
/// Returns the record and every write it leaves out with why, sorted in history
/// order. `policy` decides whether a write left out for any reason other than a
/// fork aborts. With `signers`, a write still waiting at the end that is stuck behind a change its
/// writer never sent is named so ([`UnmergedReason::BehindUnsent`]). A write built on a change of
/// `refused` is left out whatever `policy` is ([`UnmergedReason::BuiltOnRefused`]), and its
/// changes join `refused`.
pub(super) fn merge_writes(
    merged: AutoCommit,
    writes: Vec<(usize, DocumentMutation)>,
    policy: LeftOut,
    signers: Option<&Signers<'_>>,
    refused: &mut RefusedChanges,
) -> Result<(AutoCommit, Vec<LeftWrite>), String> {
    let Some(rule) = refused.1 else {
        return merge_writes_as_they_come(merged, writes, policy, signers, refused);
    };
    // The record's own rules read each write as it stands among the changes it builds on, which
    // the merged record holds whatever order the writes came in: a write they refuse is left out,
    // with every write built on it, and the rest merged again without them.
    let base = merged.clone();
    let all = writes.clone();
    let (mut merged, left) = merge_writes_as_they_come(merged, writes, policy, signers, refused)?;
    let not_merged: std::collections::HashSet<usize> =
        left.iter().map(|(index, _, _)| *index).collect();
    let mut ruled_out: Vec<(usize, String)> = Vec::new();
    for (index, mutation) in &all {
        let DocumentMutation::Incremental(bytes) = mutation else {
            continue;
        };
        if not_merged.contains(index) {
            continue;
        }
        let changes: Vec<automerge::Change> = tilo_automerge_isolate::change_hashes(bytes)
            .unwrap_or_default()
            .iter()
            .filter_map(|hash| merged.get_change_by_hash(hash))
            .collect();
        if let Some(why) = changes
            .iter()
            .find_map(|change| rule(&merged, change).err())
        {
            ruled_out.push((*index, why));
        }
    }
    if ruled_out.is_empty() {
        return Ok((merged, left));
    }
    let mut kept = Vec::with_capacity(all.len());
    let mut refused_now = Vec::with_capacity(ruled_out.len());
    for (index, mutation) in all {
        match ruled_out.iter().find(|(out, _)| *out == index) {
            Some((_, why)) => {
                refused.refuse(&mutation);
                refused_now.push((
                    index,
                    UnmergedReason::Refused {
                        refusal: why.clone(),
                    },
                    mutation,
                ));
            }
            None => kept.push((index, mutation)),
        }
    }
    let (merged, mut left) = merge_writes(base, kept, policy, signers, refused)?;
    left.extend(refused_now);
    left.sort_by_key(|(index, _, _)| *index);
    Ok((merged, left))
}

/// [`merge_writes`] by the workspace's rules alone, each write decided before it came here.
fn merge_writes_as_they_come(
    mut merged: AutoCommit,
    writes: Vec<(usize, DocumentMutation)>,
    policy: LeftOut,
    signers: Option<&Signers<'_>>,
    refused: &mut RefusedChanges,
) -> Result<(AutoCommit, Vec<LeftWrite>), String> {
    let (mut left, writes) = refused.left_out(writes);
    if merged_at_once(&mut merged, &writes) {
        return Ok((merged, left));
    }
    let mut waiting = writes;
    let mut stuck = Vec::new();
    // Writes refused for writing a sequence the record holds that also wait for a change it
    // lacks: a writer that lost a change and wrote again from the record as it read without it
    // numbered its next change as the lost one. Decided below with the writes still waiting.
    let mut forks = Vec::new();
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
                    if signers.is_some() && waits_for_a_missing_change(&merged, &mutation) {
                        forks.push((index, actor, seq, mutation));
                        continue;
                    }
                    UnmergedReason::AlreadyHeld { actor, seq }
                }
                WriteMerge::Refused(why) => UnmergedReason::Unreadable(why),
                // This device could not decide it: no fact about the write.
                WriteMerge::Device(reason) => return Err(reason),
            };
            if policy == LeftOut::ForkOnly && !matches!(reason, UnmergedReason::AlreadyHeld { .. })
            {
                return Err(format!(
                    "a write of the sealed history does not merge: {reason}"
                ));
            }
            left.push((index, reason, mutation));
        }
        if next.len() == before {
            stuck = next;
            break;
        }
        waiting = next;
    }
    if stuck.is_empty() && forks.is_empty() {
        left.sort_by_key(|(index, _, _)| *index);
        return Ok((merged, left));
    }
    let missing = if stuck.is_empty() {
        Vec::new()
    } else {
        missing_dependencies(&merged, &stuck)?
    };
    let unsent = match signers {
        Some(signers) => {
            let waiting: Vec<_> = stuck
                .iter()
                .map(|(index, mutation)| (*index, mutation))
                .chain(
                    forks
                        .iter()
                        .map(|(index, _, _, mutation)| (*index, mutation)),
                )
                .map(|(index, mutation)| ((signers.of)(index), mutation))
                .collect();
            super::unsent_bases::behind_unsent(&merged, &waiting, &(signers.held)())
        }
        None => vec![false; stuck.len().saturating_add(forks.len())],
    };
    let (stuck_unsent, forks_unsent) = unsent.split_at(stuck.len().min(unsent.len()));
    for ((index, mutation), unsent) in stuck.into_iter().zip(stuck_unsent) {
        let missing = missing.clone();
        let reason = if *unsent {
            UnmergedReason::BehindUnsent { missing }
        } else {
            UnmergedReason::MissingDependencies { missing }
        };
        if policy == LeftOut::ForkOnly {
            return Err(format!(
                "a write of the sealed history does not merge: {reason}"
            ));
        }
        left.push((index, reason, mutation));
    }
    for ((index, actor, seq, mutation), unsent) in forks.into_iter().zip(forks_unsent) {
        let reason = if *unsent {
            UnmergedReason::BehindUnsent {
                missing: missing_of(&merged, &mutation),
            }
        } else {
            UnmergedReason::AlreadyHeld { actor, seq }
        };
        left.push((index, reason, mutation));
    }
    left.sort_by_key(|(index, _, _)| *index);
    Ok((merged, left))
}

/// Merge every one of `writes` over `document` in one go, when they all merge: none waiting for
/// a change the document and the others lack, none writing a sequence the document already
/// holds under another change. Whether they did; when they did not, `document` is left as it
/// was and each is decided on its own ([`merge_writes`]).
///
/// Merged one by one, every write was checked on its own against the document as it stood, each
/// a round trip to the isolate that decodes untrusted changes: a mailbox's part of a hundred
/// and thirty messages took seconds to open, and a server that restarted opened every part
/// of the mailbox before it answered the import's next delivery.
fn merged_at_once(document: &mut AutoCommit, writes: &[(usize, DocumentMutation)]) -> bool {
    if writes.len() < 2 {
        return false;
    }
    let merges = |candidate: &mut AutoCommit, mutation: DocumentMutation| {
        matches!(
            merge_committed(candidate, &mutation),
            Ok(crate::document::MergeOutcome::Changed(_))
        )
    };
    let mut candidate = document.clone();
    let mut changes = Vec::new();
    for (_, mutation) in writes {
        match mutation {
            DocumentMutation::Incremental(bytes) => changes.extend_from_slice(bytes),
            // A whole document (the write that founded it) is merged on its own, after
            // the changes before it.
            DocumentMutation::Snapshot(_) => {
                if !changes.is_empty()
                    && !merges(
                        &mut candidate,
                        DocumentMutation::Incremental(std::mem::take(&mut changes)),
                    )
                {
                    return false;
                }
                if !merges(&mut candidate, mutation.clone()) {
                    return false;
                }
            }
        }
    }
    if !changes.is_empty() && !merges(&mut candidate, DocumentMutation::Incremental(changes)) {
        return false;
    }
    *document = candidate;
    true
}

/// The changes `mutation`'s changes build on that are in neither `record` nor `mutation` itself,
/// hex.
fn missing_of(record: &AutoCommit, mutation: &DocumentMutation) -> Vec<String> {
    let DocumentMutation::Incremental(bytes) = mutation else {
        return Vec::new();
    };
    let changes = tilo_failure::ignored!(
        "a write whose changes do not parse waits for nothing this names",
        tilo_automerge_isolate::parse_changes(bytes).unwrap_or_default()
    );
    let own: std::collections::HashSet<automerge::ChangeHash> =
        changes.iter().map(automerge::Change::hash).collect();
    let document = record.committed_document();
    let mut missing: Vec<String> = changes
        .iter()
        .flat_map(|change| change.deps().iter().copied())
        .filter(|dep| !own.contains(dep) && !automerge::ReadDoc::has_change(document, dep))
        .map(|dep| dep.to_string())
        .collect();
    missing.dedup();
    missing
}

/// Whether `mutation` builds on a change in neither `record` nor itself.
fn waits_for_a_missing_change(record: &AutoCommit, mutation: &DocumentMutation) -> bool {
    !missing_of(record, mutation).is_empty()
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

/// What decides each write a reader opened before it is merged: the check admission runs on the
/// same write in the clear ([`StatePeer::check_opened_write`]).
pub(super) type CheckOpened<'a> = dyn FnMut(&SignedOp, &DocumentMutation) -> Result<(), super::document_capture::OpenedRefusal>
    + 'a;

/// `merge_sealed` for reading: every write that opens, passes `check` and merges is in the
/// record, and the rest are named instead of refusing the whole record. One write that never
/// arrived, does not open, or that the workspace's rules refuse must not leave a workspace unable
/// to read its record, and the writes after it are kept, not dropped. A write `check` cannot
/// decide here and now fails the read: it is still arriving, and leaving it out would read the
/// record as no other device does.
#[cfg(test)]
pub(super) fn open_sealed(
    document: &super::Document,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    check: &mut CheckOpened<'_>,
) -> Result<OpenedDocument, String> {
    open_sealed_after(document, open, None, check, &|_| false, &|_, _| Ok(None))
        .map(|(opened, _)| opened)
        .map_err(|error| error.to_string())
}

/// An opening that took no write since the last: what that one opened, and what it merged.
type Unchanged = Box<(OpenedDocument, Option<MergedDocument>)>;

/// Where an opening of `sealed` goes on from `prior`: the first write it has not merged and what
/// it merged, or the whole history from the document itself. When nothing was written since,
/// what the last read made, as the opening (`Err`).
fn continued(
    document: &super::Document,
    sealed: &[&SignedOp],
    base: &[automerge::ChangeHash],
    prior: Option<MergedDocument>,
) -> Result<(usize, AutoCommit), Unchanged> {
    let prior = match prior.filter(|prior| prior.continued_by(base, sealed)) {
        // Nothing written since: it reads as the last read made it.
        Some(prior) if prior.through == sealed.len() => {
            let document = prior.saved.clone();
            return Err(Box::new((
                OpenedDocument {
                    document,
                    unmerged: Vec::new(),
                },
                Some(prior),
            )));
        }
        prior => prior,
    };
    Ok(
        match prior.and_then(|prior| Some((prior.through, prior.merged?))) {
            Some(continued) => continued,
            None => (0, (*document.doc).clone()),
        },
    )
}

/// Why a sealed record does not open here and now.
#[derive(Debug)]
pub enum OpenSealedError {
    /// The checkpoint leading its history waits for writes it stands in for that are not here
    /// yet: the record is incomplete until they arrive, never refused.
    Incomplete,
    /// It does not open: a write fails to open or merge, or cannot be decided here.
    Failed(String),
}

impl std::fmt::Display for OpenSealedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Incomplete => f.write_str(super::checkpoints::WAITS_FOR_ITS_WRITES),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for OpenSealedError {}

impl From<String> for OpenSealedError {
    fn from(why: String) -> Self {
        Self::Failed(why)
    }
}

/// `open_sealed` over what `prior`, an earlier opening of the document, merged: where the
/// document only took writes since (its base and the history `prior` merged are as they were),
/// only those are opened and merged, so reading a document its writes keep growing costs the
/// writes it took, not its whole history each time. Also what this opening merged, for the next one, unless it
/// left a write out or a write waits for segments: such a write may merge once more arrive, and
/// only an opening of the whole history takes it in then. A checkpoint leading the history is
/// read against the writes it stands in for ([`super::checkpoints::read_checkpoint`]), `settled`
/// saying whether every device with the record's keys checked it.
pub(super) fn open_sealed_after(
    document: &super::Document,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    prior: Option<MergedDocument>,
    check: &mut CheckOpened<'_>,
    settled: &dyn Fn(&SignedOp) -> bool,
    waits: &WaitsForAction<'_>,
) -> Result<(OpenedDocument, Option<MergedDocument>), OpenSealedError> {
    let sealed: Vec<&SignedOp> = document.sealed.iter().collect();
    let base = document.doc.committed_document().get_heads();
    let (from, merged) = match continued(document, &sealed, &base, prior) {
        Ok(continued) => continued,
        Err(unchanged) => return Ok(*unchanged),
    };
    let mut unmerged = Vec::new();
    let mut writes = Vec::new();
    let mut refused = RefusedChanges::of(document);
    // Writes the checkpoint stands in for that the workspace's rules refuse: no write of the
    // history, so named apart.
    let mut beside = Vec::new();
    let mut transfers = crate::document_frame::Transfers::default();
    // From a write whose action is not whole here on, the record reads without its writes, as a
    // read in turns does ([`super::document_capture`]).
    let mut waiting: Option<String> = None;
    for (index, write) in sealed.iter().enumerate().skip(from) {
        let opened = open(write).and_then(|plain| {
            if waiting.is_none() {
                waiting = waits(write, &plain)?;
            }
            Ok(plain)
        });
        if let Some(action) = &waiting {
            let action = action.clone();
            unmerged.push((index, UnmergedReason::WaitsForAction { action }));
            continue;
        }
        match opened.and_then(|plain| transfers.take(*write.author.as_bytes(), &plain)) {
            Ok(Some(mutation)) if write.kind == OpKind::Compaction => {
                let mutation = checkpoint_write(
                    document,
                    mutation,
                    &mut *open,
                    &mut *check,
                    settled,
                    &mut refused,
                    &mut beside,
                )?;
                writes.push((index, mutation));
            }
            Ok(Some(mutation)) => match check(write, &mutation) {
                Ok(()) => writes.push((index, mutation)),
                Err(super::document_capture::OpenedRefusal::Refused(refusal)) => {
                    refused.refuse(&mutation);
                    unmerged.push((
                        index,
                        UnmergedReason::Refused {
                            refusal: refusal.reason,
                        },
                    ));
                }
                Err(refusal @ super::document_capture::OpenedRefusal::CannotDecide(_)) => {
                    return Err(OpenSealedError::Failed(refusal.to_string()));
                }
            },
            Ok(None) => {}
            Err(why) => unmerged.push((index, UnmergedReason::Unreadable(why))),
        }
    }
    let signed: Vec<super::unsent_bases::Signed> = sealed
        .iter()
        .map(|op| super::unsent_bases::Signed {
            author: *op.author.as_bytes(),
            seq: op.seq,
        })
        .collect();
    let signers = Signers {
        of: &|index| signed.get(index).copied(),
        held: &|| super::unsent_bases::HeldThrough::of(document),
    };
    let (mut merged, left) =
        merge_writes(merged, writes, LeftOut::Named, Some(&signers), &mut refused)?;
    unmerged.extend(left.into_iter().map(|(index, reason, _)| (index, reason)));
    unmerged.sort_by_key(|(index, _)| *index);
    let saved = merged.save_with_options(automerge::SaveOptions {
        retain_orphans: false,
        ..automerge::SaveOptions::default()
    });
    let next = (unmerged.is_empty() && transfers.is_empty()).then(|| MergedDocument {
        base,
        first: sealed.first().map(|write| Mark::of(write)),
        through: sealed.len(),
        last: sealed.last().map(|write| Mark::of(write)),
        saved: saved.clone(),
        merged: Some(merged),
    });
    let opened = OpenedDocument {
        document: saved,
        unmerged: beside
            .into_iter()
            .chain(unmerged.into_iter().filter_map(|(index, reason)| {
                let write = sealed.get(index)?;
                Some(UnmergedWrite {
                    author: *write.author.as_bytes(),
                    seq: write.seq,
                    reason,
                })
            }))
            .collect(),
    };
    Ok((opened, next))
}

/// The write a checkpoint stands in for, its refusals taken: [`OpenSealedError`] where the
/// checkpoint waits on a write or cannot be decided.
fn checkpoint_write(
    document: &super::Document,
    mutation: DocumentMutation,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    check: &mut CheckOpened<'_>,
    settled: &dyn Fn(&SignedOp) -> bool,
    refused: &mut RefusedChanges,
    beside: &mut Vec<UnmergedWrite>,
) -> Result<DocumentMutation, OpenSealedError> {
    let read = super::checkpoints::read_checkpoint(document, &mutation, open, check, settled)
        .map_err(|refusal| match refusal {
            super::checkpoints::CheckpointRefusal::Waits => OpenSealedError::Incomplete,
            super::checkpoints::CheckpointRefusal::CannotDecide(why) => {
                OpenSealedError::Failed(why)
            }
        })?;
    read.refuse_in(refused);
    beside.extend(read.named().iter().cloned());
    Ok(read.instead.unwrap_or(mutation))
}

/// A sealed document as an opening of it left it, every write of its history merged
/// ([`HeldDocument::opened_over`]): a later opening of the document that only took writes since
/// merges just those over it.
pub struct MergedDocument {
    /// The heads of the document's cleartext base it was opened over.
    base: Vec<automerge::ChangeHash>,
    /// The first write of the history it merged, and its last.
    first: Option<Mark>,
    last: Option<Mark>,
    /// How many writes of the history it merged.
    through: usize,
    /// The document with them merged, saved.
    saved: Vec<u8>,
    /// The document with them merged, while it is kept ([`Self::lighten`]).
    merged: Option<AutoCommit>,
}

impl Clone for MergedDocument {
    /// A copy keeps what was read, not the document to merge further writes into: that is the one
    /// kept apart.
    fn clone(&self) -> Self {
        Self {
            base: self.base.clone(),
            first: self.first,
            last: self.last,
            through: self.through,
            saved: self.saved.clone(),
            merged: None,
        }
    }
}

impl MergedDocument {
    /// Keep only what was read: an opening after it reads it again only while the document took
    /// no write since, and opens the whole history once one comes. The merged document is the
    /// larger part by far; a reader keeps it for the documents being written.
    pub fn lighten(&mut self) {
        self.merged = None;
    }

    /// Whether it still holds the merged document a later write merges into.
    #[must_use]
    pub fn holds_merged(&self) -> bool {
        self.merged.is_some()
    }

    /// Whether the document with base heads `base` and history `sealed` is this one with only
    /// writes added after it: a compaction replaces the history's start, and a new base is
    /// another state of it.
    fn continued_by(&self, base: &[automerge::ChangeHash], sealed: &[&SignedOp]) -> bool {
        if self.base != base || sealed.len() < self.through {
            return false;
        }
        let Some(last) = self.through.checked_sub(1) else {
            return true;
        };
        sealed.first().map(|write| Mark::of(write)) == self.first
            && sealed.get(last).map(|write| Mark::of(write)) == self.last
    }
}

/// One write of a sealed history, told apart from every other: its writer, its sequence, and its
/// signature.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Mark([u8; 32], u64, collab_types::crypto::Signature);

impl Mark {
    fn of(write: &SignedOp) -> Self {
        Self(*write.author.as_bytes(), write.seq, write.signature)
    }
}

/// A document as a state held it once ([`PeerPublication::held_document`]),
/// opened without holding the state: with a copy of the state it was taken from,
/// whose rules decide each write opened as every other reader's do.
pub struct HeldDocument {
    document: super::backend::StoredDocument,
    route: Route,
    peer: Box<StatePeer>,
}

impl HeldDocument {
    /// The document as its key holder reads it
    /// ([`PeerPublication::opened_document`]): one that is not encrypted reads
    /// as it is saved.
    ///
    /// # Errors
    ///
    /// A sealed write's opening, or the merge of the writes, fails.
    pub fn opened(
        &self,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    ) -> Result<OpenedDocument, String> {
        if !self.document.encryption_required {
            return Ok(OpenedDocument {
                document: self.document.doc.saved(),
                unmerged: Vec::new(),
            });
        }
        open_sealed_after(
            &self.document,
            open,
            None,
            &mut |write, mutation| self.check(write, mutation),
            &|checkpoint| self.settled(checkpoint),
            &|write, plaintext| self.waits(write, plaintext),
        )
        .map(|(opened, _)| opened)
        .map_err(|error| error.to_string())
    }

    /// Whether every device with the record's keys checked `checkpoint`, which leads the document's
    /// history: it is then read as itself where the writes it stands in for are not here.
    fn settled(&self, checkpoint: &SignedOp) -> bool {
        self.peer
            .checkpoint_settled((&self.route.app, &self.route.record), checkpoint)
    }

    /// The action `write`, opened as `plaintext`, waits for the rest of.
    fn waits(&self, write: &SignedOp, plaintext: &[u8]) -> Result<Option<String>, String> {
        self.peer
            .sealed_action_waits((&self.route.app, &self.route.record), write, plaintext)
    }

    /// Decide `write`, opened as `mutation`, as every reader of the document does.
    fn check(
        &self,
        write: &SignedOp,
        mutation: &DocumentMutation,
    ) -> Result<(), super::document_capture::OpenedRefusal> {
        self.peer
            .check_write_in_log((&self.route.app, &self.route.record), write, mutation)
    }

    /// [`Self::opened`] over `prior`, what an earlier opening of this document left
    /// ([`open_sealed_after`]), and what this one leaves for the next: `None` for a document
    /// written in the clear, which reads as it is saved at no cost.
    ///
    /// # Errors
    ///
    /// [`OpenSealedError::Incomplete`] while the checkpoint leading its history waits for the
    /// writes it stands in for; [`OpenSealedError::Failed`] as [`Self::opened`] fails.
    pub fn opened_over(
        &self,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
        prior: Option<MergedDocument>,
    ) -> Result<(OpenedDocument, Option<MergedDocument>), OpenSealedError> {
        if !self.document.encryption_required {
            return Ok((self.opened(open)?, None));
        }
        open_sealed_after(
            &self.document,
            open,
            prior,
            &mut |write, mutation| self.check(write, mutation),
            &|checkpoint| self.settled(checkpoint),
            &|write, plaintext| self.waits(write, plaintext),
        )
    }

    /// Whether its writes are sealed.
    #[must_use]
    pub fn is_sealed(&self) -> bool {
        self.document.encryption_required
    }
}

/// Everything a record's history holds, each write opened on its own and
/// nothing merged ([`PeerPublication::opened_history`]): what an operator
/// repairing a record that does not read whole reads it from.
#[derive(Clone, Debug)]
pub struct OpenedHistory {
    /// The record's cleartext base, saved: the whole record for one written in
    /// the clear.
    pub base: Vec<u8>,
    /// The record's sealed writes, in history order.
    pub writes: Vec<OpenedWrite>,
}

/// One write of a sealed history, opened.
#[derive(Clone, Debug)]
pub struct OpenedWrite {
    pub author: [u8; 32],
    pub seq: u64,
    /// What it writes, or why it does not open.
    pub mutation: Result<DocumentMutation, String>,
}

impl PeerPublication<'_> {
    /// The cleartext base of `record` of `app` and every write of its sealed
    /// history opened with `open`, none merged: `None` for a record this state
    /// does not hold. A record written in the clear has no writes.
    pub fn opened_history(
        self,
        app: &str,
        record: &str,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    ) -> Option<OpenedHistory> {
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let document = self.peer.core.documents.get(&route)?;
        let writes = if document.encryption_required {
            // A write in segments is one write, read where its last segment is.
            let mut transfers = crate::document_frame::Transfers::default();
            document
                .sealed
                .iter()
                .filter_map(|write| {
                    let mutation = open(write)
                        .and_then(|plain| transfers.take(*write.author.as_bytes(), &plain))
                        .transpose()?;
                    Some(OpenedWrite {
                        author: *write.author.as_bytes(),
                        seq: write.seq,
                        mutation,
                    })
                })
                .collect()
        } else {
            Vec::new()
        };
        Some(OpenedHistory {
            base: document.doc.saved(),
            writes,
        })
    }

    /// `record` of `app` as its channel's key holder reads it: the cleartext
    /// base with every sealed write `open` opens merged over it, and the
    /// writes it leaves out (see [`OpenedDocument`]). A record that is not
    /// encrypted reads as [`StatePeer::document`]. `None` for a record this state
    /// does not hold.
    pub fn opened_document(
        self,
        app: &str,
        record: &str,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    ) -> Result<Option<OpenedDocument>, String> {
        self.held_document(app, record)
            .map(|held| held.opened(open))
            .transpose()
    }

    /// The document `id` of `app` as this state holds it now, to be opened
    /// apart from it ([`HeldDocument::opened`]): taking it costs a pointer per
    /// chunk of its history, while opening a sealed one decrypts and merges
    /// every write it holds. Whoever reads the state under a lock takes the
    /// document, lets go of the lock and only then opens it, so the replica's
    /// other readers and writers never wait on a document's whole history.
    /// `None` for a document this state does not hold.
    #[must_use]
    pub fn held_document(self, app: &str, id: &str) -> Option<HeldDocument> {
        let route = Route {
            app: app.into(),
            record: id.into(),
        };
        let document = self.peer.core.documents.get(&route)?.clone();
        Some(HeldDocument {
            document,
            route,
            peer: Box::new(self.peer.clone()),
        })
    }

    /// Whether `record` of `app` is encrypted here: its writes are sealed,
    /// and a write in the clear is refused as a downgrade.
    #[must_use]
    pub fn is_sealed_document(self, app: &str, record: &str) -> bool {
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
    pub fn publish_sealed_incremental(
        self,
        app: &str,
        record: &str,
        changes: Vec<u8>,
        seal: &mut SealCheckpoint<'_>,
        key: &dyn DeviceSigner,
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
        key: &dyn DeviceSigner,
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
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let plaintext = mutation.encode().map_err(err)?;
        let mut op = self.peer.draft_write(
            (app, record),
            None,
            &plaintext,
            Some(seal),
            key.device_public_key(),
            0,
            now,
        )?;
        let author = op.author;
        collab_types::sign_op(&mut op, key).map_err(err)?;
        let mut candidate = self.peer.clone();
        candidate.admit_new_local_document(app, record, op.clone(), now)?;
        *self.peer = candidate;
        Ok(NeighbourMsg::AppDocument {
            peer: peer_handle(author.as_bytes()),
            app_id: app.into(),
            cluster_id: self.peer.core.workspace.clone(),
            document_id: record.into(),
            readers: None,
            envelope: Box::new(op),
        })
    }
}

impl PeerPublication<'_> {
    /// Every encrypted record whose sealed history has used three quarters
    /// of its bound, as `(app, record)`: its key holder compacts it before
    /// new writes are refused. A document holding writes sealed before
    /// reader sets is due at once ([`Self::awaiting_reseal`]): the compaction
    /// is what seals their content again under the document's own scope.
    #[must_use]
    pub fn sealed_to_compact(self) -> Vec<(String, String)> {
        let mut due: Vec<(String, String)> = self
            .peer
            .core
            .documents
            .iter()
            .filter(|(route, document)| {
                document.sealed.due() || self.holds_sealed_before_reader_sets(route, document)
            })
            .map(|(route, _)| route.named())
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
                    .filter(|(_, document)| document.sealed.writes().0 >= PRESSED_HISTORY)
                    .max_by_key(|(_, document)| document.sealed.writes().0)
                    .map(|(route, _)| route.named()),
            );
        }
        due
    }

    /// Whether `document` of `app` holds writes sealed before reader sets that no key holder has
    /// sealed again under the document's own scope yet
    /// ([`collab_types::artifact_before_reader_sets`]). A replica holding the document's
    /// keys opens them as they are, and its compaction seals their content again
    /// ([`Self::sealed_to_compact`]); one without them reads none of it until a key holder has.
    #[must_use]
    pub fn awaiting_reseal(self, app: &str, document: &str) -> bool {
        let route = Route::of(app, document);
        self.peer
            .core
            .documents
            .get(&route)
            .is_some_and(|held| self.holds_sealed_before_reader_sets(&route, held))
    }

    /// The documents holding writes sealed before reader sets that no key holder has sealed
    /// again yet ([`Self::awaiting_reseal`]), as `(app, document)`.
    #[must_use]
    pub fn awaiting_reseals(self) -> Vec<(String, String)> {
        self.peer
            .core
            .documents
            .iter()
            .filter(|(route, held)| self.holds_sealed_before_reader_sets(route, held))
            .map(|(route, _)| route.named())
            .collect()
    }

    /// The writes of `document` of `app` sealed before reader sets whose content a key holder
    /// sealed again, each as its author signed it: kept beside the document so who wrote what
    /// stays provable, and never replayed as its history.
    #[must_use]
    pub fn resealed_originals(self, app: &str, document: &str) -> Vec<SignedOp> {
        self.peer
            .core
            .documents
            .get(&Route::of(app, document))
            .map(|held| held.resealed.to_vec())
            .unwrap_or_default()
    }

    fn holds_sealed_before_reader_sets(self, route: &Route, held: &super::Document) -> bool {
        held.sealed
            .iter()
            .any(|write| route.sealed_before_reader_sets(&self.peer.core.workspace, write))
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
                        document_id: route.record.clone(),
                        envelope: Box::new(op.clone()),
                        readers: None,
                    });
                }
            }
        }
        messages
    }
}

/// Whether `compaction` is sealed under an earlier epoch of the keys one of the `writes` it
/// folds was sealed under: a member a later epoch left out would read in it what that write kept
/// from them.
///
/// Epochs count within one set of keys, so only writes sealed under the compaction's own keys
/// are compared with it: the workspace's, or the one document group it names. A document that
/// moved to a group of its own (its reader set, or writes sealed before reader sets that a key
/// holder seals again) is compacted under that group, at whatever epoch the group has reached,
/// over writes the workspace's keys sealed at theirs; which group a compaction may be sealed
/// under at all is the document's key scope's to say ([`StatePeer::check_document_key_scope`]).
fn sealed_under_a_stale_epoch<'a>(
    compaction: &SignedOp,
    writes: impl IntoIterator<Item = &'a SignedOp>,
) -> bool {
    let Some(sealed) = compaction.encryption else {
        return writes
            .into_iter()
            .filter_map(|write| write.encryption)
            .any(|write| write.epoch > 0);
    };
    writes
        .into_iter()
        .filter_map(|write| write.encryption)
        .filter(|write| key_group_of(write) == key_group_of(&sealed))
        .any(|write| write.epoch > sealed.epoch)
}

/// The document group `sealed` names, or `None` for the workspace's own keys.
const fn key_group_of(
    sealed: &collab_types::EncryptionMetadata,
) -> Option<collab_types::WorkspaceId> {
    match sealed.scheme {
        collab_types::EncryptionScheme::DocumentContentKey { group, .. } => Some(group),
        collab_types::EncryptionScheme::ContentKey { .. }
        | collab_types::EncryptionScheme::MlsApplication => None,
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
    segments: Vec<SignedOp>,
) -> Result<Vec<SignedOp>, String> {
    let wrapper = SealedCompaction::parse(compaction).map_err(err)?;
    if !numbered_under_its_folds(compaction, &wrapper) {
        return Err(
            "sealed compaction is not numbered as the last write of its author it folds".into(),
        );
    }
    if history.is_empty() {
        // Nothing of this route is here to supersede: its key holder's
        // compaction is the whole record, the first thing a replica that
        // holds none of it receives, as a device catching up from a store
        // that keeps only ciphertext does. A replica that holds the route from
        // the governance's coverage holds the floors that says it reached and
        // none of its writes; there is nothing of it to hide from, and the
        // compaction is how the store hands it over.
        return Ok(std::iter::once(compaction.clone())
            .chain(segments)
            .collect());
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
    if sealed_under_a_stale_epoch(compaction, superseded.iter().copied()) {
        return Err("sealed compaction is sealed under a stale epoch".into());
    }
    Ok(std::iter::once(compaction.clone())
        .chain(segments)
        .chain(kept.into_iter().cloned())
        .collect())
}

/// Re-validate a stored history's compaction: only ever its first entry,
/// covering nothing beyond the record's floors and none of the later writes.
pub(super) fn check_stored(history: &[SignedOp], floors: &Floors) -> Result<(), String> {
    let segments = super::compaction_set::held_segments(history)?;
    for (index, op) in history.iter().enumerate() {
        if op.kind != OpKind::Compaction || (1..=segments).contains(&index) {
            continue;
        }
        if index != 0 {
            return Err("a sealed compaction is only ever first in a record's history".into());
        }
        let wrapper = SealedCompaction::parse(op).map_err(err)?;
        if !numbered_under_its_folds(op, &wrapper) {
            return Err(
                "stored sealed compaction is not numbered as the last write of its author it folds"
                    .into(),
            );
        }
        if covers_beyond(&wrapper, floors) {
            return Err("stored sealed compaction is ahead of the record's watermark".into());
        }
        if history
            .iter()
            .skip(segments.saturating_add(1))
            .any(|write| wrapper.covers(write))
        {
            return Err("stored sealed compaction covers a later write".into());
        }
    }
    Ok(())
}

/// Whether `compaction` is numbered as the last write of its author it folds,
/// 0 when it folds none of theirs: the number its readers' floor then takes,
/// so a page that opens the record by the fold still takes the author's next
/// write.
fn numbered_under_its_folds(compaction: &SignedOp, wrapper: &SealedCompaction) -> bool {
    compaction.seq == wrapper.covered(&compaction.author).unwrap_or(0)
}

fn covers_beyond(wrapper: &SealedCompaction, floors: &Floors) -> bool {
    wrapper
        .covers
        .iter()
        .any(|(author, seq)| floors.get(author).is_none_or(|floor| floor < seq))
}
