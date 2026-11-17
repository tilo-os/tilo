//! The durable record of a replica, and restoring one from it.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::{
    AcceptedCheckpoint, CachedDocument, DecodedCheckpoint, DeviceCert, Document, Ed25519PublicKey,
    ExactAdmissions, InboundState, PeerCore, RevocationList, Route, Shared, StatePeer, SystemTime,
    backend, check_governed_snapshot, err, held, now_ns, process_default,
};
use super::{DurableDocument, SealedHistory};
use crate::NeighbourMsg;

#[cfg(test)]
use automerge::{ReadDoc as _, transaction::Transactable as _};

/// The durable layout this build writes and reads. A change to what is
/// stored bumps this: a build refuses state of another version by name
/// rather than reading it as this one's.
pub const DURABLE_VERSION: u32 = 9;

/// A replica's durable state. `exact` is the exact admission index, present
/// once the replica selects exact admissions; `sealed` holds each encrypted
/// record's sealed writes, empty when no record has any.
#[derive(Serialize, Deserialize)]
pub struct Durable<'a> {
    pub(super) version: u32,
    pub(super) workspace: String,
    pub(super) owner: [u8; 32],
    pub(super) workspace_bytes: Option<Vec<u8>>,
    pub(super) documents: Vec<DurableDocument>,
    pub(super) receipts: BTreeMap<String, Vec<u8>>,
    pub(super) certificates: Vec<DeviceCert>,
    pub(super) revocations: Vec<tilo_user_identity::RevocationEntry>,
    pub(super) pending: VecDeque<([u8; 32], NeighbourMsg, bool)>,
    pub(super) pending_bytes: usize,
    #[serde(borrow)]
    pub(super) exact: Option<&'a [u8]>,
    pub(super) sealed: SealedHistory,
}

impl<'a> Durable<'a> {
    /// The durable state `bytes` hold, of `workspace` owned by `owner`.
    fn read(bytes: &'a [u8], workspace: &str, owner: [u8; 32]) -> Result<Self, String> {
        let (version, _) = postcard::take_from_bytes::<u32>(bytes).map_err(err)?;
        if version != DURABLE_VERSION {
            return Err(if version < DURABLE_VERSION {
                format!(
                    "this state is from an older format (version {version}, and this build \
                     reads version {DURABLE_VERSION}): start fresh with a new state directory"
                )
            } else {
                format!(
                    "this state is from a newer format (version {version}, and this build \
                     reads version {DURABLE_VERSION}): run a newer build, or start fresh with a \
                     new state directory"
                )
            });
        }
        // Bytes of this version that do not decode are of another layout that
        // called itself this version: said plainly, never as a decode's words
        // alone.
        let (durable, tail): (Self, _) = postcard::take_from_bytes(bytes).map_err(|error| {
            format!(
                "this state cannot be read by this build (it may be from another format): \
                 {error}. Start fresh with a new state directory"
            )
        })?;
        if !tail.is_empty() {
            return Err("trailing bytes after durable state".into());
        }
        if !durable.sealed.is_empty() && durable.exact.is_none() {
            return Err("sealed record history requires exact admissions".into());
        }
        if durable.workspace != workspace || durable.owner != owner {
            return Err("durable admission namespace mismatch".into());
        }
        Ok(durable)
    }
}

/// A stretch of the durable encoding ([`StatePeer::durable_parts`]).
#[derive(Clone)]
pub(super) enum Piece {
    /// A part with bytes of its own.
    Part(std::sync::Arc<[u8]>),
    /// A document's changes since its base, in runs: one stretch, as a restored document holds
    /// them in one run.
    Since(Vec<std::sync::Arc<[u8]>>),
    /// The length and digest of a stretch whose bytes are not kept: what a session that stores
    /// nothing makes of its records' parts to hash them.
    Digested(u64, [u8; 32]),
}

/// Whether [`StatePeer::durable_pieces`] keeps the encoding's parts or only their digests.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Digests {
    /// Keep the parts: a store writes them.
    Kept,
    /// Keep only lengths and digests: nothing here is stored, so a session pays for a hash, not
    /// for a second copy of every record beside the replica it holds.
    Taken,
}

impl StatePeer {
    /// A governed assertion alone is insufficient: bind it to the accepted exact checkpoint.
    pub fn durable_coverage(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<crate::durability::DurableReceipt>, String> {
        Ok(self
            .durable_receipt_checkpoint(app, record)?
            .map(|(receipt, _)| receipt))
    }
    pub(super) fn durable_receipt_checkpoint(
        &self,
        app: &str,
        record: &str,
    ) -> Result<
        Option<(
            crate::durability::DurableReceipt,
            std::sync::Arc<DecodedCheckpoint>,
        )>,
        String,
    > {
        let Some(workspace) = self.core.workspace_doc.as_deref() else {
            return Ok(None);
        };
        let Some(receipt) = self.durable_receipt(app, record)? else {
            return Ok(None);
        };
        let coverage = &receipt.coverage;
        let Some(document) = self.core.documents.get(&Route {
            app: app.into(),
            record: record.into(),
        }) else {
            return Ok(None);
        };
        if document.encryption_required {
            return Ok(None);
        }
        let Some(snapshot) = document.covered_snapshot.as_ref() else {
            return Ok(None);
        };
        if crate::coverage::entries(workspace)?.get(&crate::coverage::route_key(app, record))
            != Some(coverage)
        {
            return Ok(None);
        }
        let accepted = snapshot.decoded()?;
        if accepted.heads != coverage.heads
            || accepted.payload_hash != coverage.payload_hash
            || coverage
                .watermarks
                .iter()
                .any(|(author, seq)| document.covered.get(author).copied().unwrap_or(0) < *seq)
        {
            return Ok(None);
        }
        Ok(Some((receipt, accepted)))
    }
    /// The workspace's durable receipt for a route, as governance holds it:
    /// written by any key holder or store whose device this replica knows.
    pub(super) fn durable_receipt(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<crate::durability::DurableReceipt>, String> {
        let Some(workspace) = self.core.workspace_doc.as_deref() else {
            return Ok(None);
        };
        let Some(receipt) =
            crate::durability::entries(workspace)?.remove(&crate::coverage::route_key(app, record))
        else {
            return Ok(None);
        };
        let coverage = &receipt.coverage;
        if coverage.workspace != self.core.workspace
            || coverage.app != app
            || coverage.record != record
            || !self.known_device(&coverage.author)
        {
            return Err("durable receipt authority/route mismatch".into());
        }
        Ok(Some(receipt))
    }
    /// Validate the receipt and current-head coverage against one decoded checkpoint.
    /// All authority, route, payload and watermark checks remain fresh per query.
    #[cfg(test)]
    pub fn durable_coverage_with_current(
        &self,
        app: &str,
        record: &str,
    ) -> Result<Option<(crate::durability::DurableReceipt, bool)>, String> {
        let Some((receipt, accepted)) = self.durable_receipt_checkpoint(app, record)? else {
            return Ok(None);
        };
        let document = self
            .core
            .documents
            .get(&Route {
                app: app.into(),
                record: record.into(),
            })
            .ok_or("missing record")?;
        let current = document
            .doc
            .base_heads()
            .iter()
            .all(|head| accepted.doc.get_change_by_hash(head).is_some());
        Ok(Some((receipt, current)))
    }
    /// Whether the durable receipt for the record covers its current heads.
    #[cfg(test)]
    pub fn durable_covers_current(&self, app: &str, record: &str) -> Result<bool, String> {
        Ok(self
            .durable_coverage_with_current(app, record)?
            .is_some_and(|(_, current)| current))
    }
    /// A digest of [`Self::durable`]: of the length and digest of each of
    /// its stretches ([`Piece`]), in order. The stretches are digested as they are
    /// encoded and only their lengths and digests are kept — per state of each document, so a
    /// write pays for what it changed — because a session that stores nothing would otherwise
    /// hold a second copy of every record beside the replica it holds.
    pub fn durable_hash(&self) -> Result<[u8; 32], String> {
        let mut hasher = blake3::Hasher::new();
        for piece in self.durable_pieces(Digests::Taken)? {
            let Piece::Digested(len, digest) = piece else {
                return Err("a durable hash was taken from parts".into());
            };
            hasher.update(&len.to_le_bytes());
            hasher.update(&digest);
        }
        Ok(*hasher.finalize().as_bytes())
    }
    /// [`Self::durable_hash`] over the parts themselves, as a store would write them: the same
    /// digests, taken from the bytes. What the digest path must answer.
    #[cfg(test)]
    pub(crate) fn durable_hash_of_kept_parts(&self) -> Result<[u8; 32], String> {
        let mut hasher = blake3::Hasher::new();
        for piece in self.durable_pieces(Digests::Kept)? {
            let (len, digest) = match piece {
                Piece::Part(part) => (
                    u64::try_from(part.len()).unwrap_or(u64::MAX),
                    *blake3::hash(&part).as_bytes(),
                ),
                Piece::Since(runs) => {
                    let mut digest = blake3::Hasher::new();
                    for run in &runs {
                        digest.update(run);
                    }
                    (
                        u64::try_from(runs.iter().map(|run| run.len()).sum::<usize>())
                            .unwrap_or(u64::MAX),
                        *digest.finalize().as_bytes(),
                    )
                }
                Piece::Digested(len, digest) => (len, digest),
            };
            hasher.update(&len.to_le_bytes());
            hasher.update(&digest);
        }
        Ok(*hasher.finalize().as_bytes())
    }

    /// The bytes of durable parts kept beside the replica: zero while nothing stores them.
    #[cfg(test)]
    pub(crate) fn kept_encoded_bytes(&self) -> usize {
        self.core
            .documents
            .values()
            .map(backend::StoredDocument::encoded_bytes)
            .sum()
    }

    /// Encode this peer's durable state.
    pub fn durable(&self) -> Result<Vec<u8>, String> {
        let parts = self.durable_parts()?;
        let mut bytes = Vec::with_capacity(parts.iter().map(|part| part.len()).sum());
        for part in &parts {
            bytes.extend_from_slice(part);
        }
        Ok(bytes)
    }
    /// Append the workspace's document to `head`, the durable encoding's head,
    /// as postcard writes an `Option<&[u8]>`: its stored base a part of its
    /// own, the same allocation while the base lasts, then the runs of
    /// changes since it, each a part too ([`CachedDocument::stored`]).
    /// Answers the head to go on with.
    fn put_workspace_document(
        &self,
        mut head: Vec<u8>,
        parts: &mut Vec<Piece>,
        digests: Digests,
    ) -> Result<Vec<u8>, String> {
        fn put<T: Serialize + ?Sized>(value: &T, out: Vec<u8>) -> Result<Vec<u8>, String> {
            postcard::to_extend(value, out).map_err(err)
        }
        let Some(document) = self.core.workspace_doc.as_ref() else {
            return put(&0u8, head);
        };
        let layout = document.stored_layout();
        let since_len = layout.since_len;
        head = put(&1u8, head)?;
        head = put(&layout.base_len.saturating_add(since_len), head)?;
        match digests {
            Digests::Kept => {
                let (base, since, _) = document.stored();
                parts.push(Piece::Part(head.into()));
                parts.push(Piece::Part(base));
                parts.push(Piece::Since(since));
            }
            Digests::Taken => {
                parts.push(digested(&head));
                parts.push(Piece::Digested(layout.base_len as u64, layout.base_digest));
                parts.push(Piece::Digested(since_len as u64, layout.since_digest));
            }
        }
        Ok(Vec::new())
    }

    /// [`Self::durable`] in parts, in order: concatenated, they are its
    /// bytes, and [`Self::restore`] takes them so.
    ///
    /// A workspace's history is most of its state and hardly changes per
    /// governance change, yet re-encoding all of it made every change cost
    /// the whole history. So each record's part is encoded once per state of
    /// the record, and the exact admission index's settled entries once per
    /// settling: a part nothing changed since an earlier call is the same
    /// allocation as then, and whoever stores the parts apart rewrites only
    /// what changed.
    pub fn durable_parts(&self) -> Result<Vec<std::sync::Arc<[u8]>>, String> {
        Ok(self
            .durable_pieces(Digests::Kept)?
            .into_iter()
            .flat_map(|piece| match piece {
                Piece::Part(part) => vec![part],
                Piece::Since(runs) => runs,
                Piece::Digested(..) => Vec::new(),
            })
            .collect())
    }

    /// [`Self::durable_parts`] as the stretches [`Self::durable_hash`] digests, keeping the
    /// parts only when `digests` asks for them.
    fn durable_pieces(&self, digests: Digests) -> Result<Vec<Piece>, String> {
        fn put<T: Serialize + ?Sized>(value: &T, out: Vec<u8>) -> Result<Vec<u8>, String> {
            postcard::to_extend(value, out).map_err(err)
        }
        let sealed = self
            .core
            .documents
            .values()
            .filter(|entry| !entry.sealed.is_empty())
            .count();
        if sealed > 0 && !self.core.exact_selected {
            return Err("sealed record history requires exact admissions".into());
        }
        // The fields of `Durable`, in order.
        let mut parts: Vec<Piece> = Vec::new();
        let mut head = put(
            &(DURABLE_VERSION, &self.core.workspace, &self.core.owner),
            Vec::new(),
        )?;
        head = self.put_workspace_document(head, &mut parts, digests)?;
        head = put(&self.core.documents.len(), head)?;
        parts.push(match digests {
            Digests::Kept => Piece::Part(head.into()),
            Digests::Taken => digested(&head),
        });
        self.documents_into(digests, &mut parts)?;
        let mut revocations = self.core.revocations.iter().collect::<Vec<_>>();
        revocations.sort_by_key(|entry| entry.revoked_device_public_key);
        let certificates = self
            .core
            .certificates
            .values()
            .map(AsRef::as_ref)
            .collect::<Vec<&DeviceCert>>();
        let mut tail = put(
            &(
                &*self.core.receipts,
                certificates,
                revocations,
                &self.core.pending,
                self.core.pending_bytes,
            ),
            Vec::new(),
        )?;
        tail = self.exact_into(tail, digests, &mut parts)?;
        tail = put(&sealed, tail)?;
        parts.push(match digests {
            Digests::Kept => Piece::Part(tail.into()),
            Digests::Taken => digested(&tail),
        });
        self.sealed_into(digests, &mut parts)?;
        Ok(parts)
    }

    /// Each record's part of the durable encoding, in route order: its document as a store
    /// writes it, or the lengths and digests of that document.
    fn documents_into(&self, digests: Digests, parts: &mut Vec<Piece>) -> Result<(), String> {
        for (route, entry) in &self.core.documents {
            match digests {
                Digests::Kept => {
                    let own = self.core.own.get(route);
                    let encoded = entry.encoded(route, || encode_document(route, entry, own))?;
                    parts.extend(encoded.document.iter().cloned());
                }
                Digests::Taken => {
                    let own = self.core.own.get(route);
                    let taken = entry.digests(|| digest_document(route, entry, own))?;
                    parts.extend(
                        taken
                            .iter()
                            .map(|(len, digest)| Piece::Digested(*len, *digest)),
                    );
                }
            }
        }
        Ok(())
    }

    /// The exact admission index's stretches, appended to `tail`: its flag and the settled
    /// entries, then what is left of the tail for the sealed writes after them.
    fn exact_into(
        &self,
        mut tail: Vec<u8>,
        digests: Digests,
        parts: &mut Vec<Piece>,
    ) -> Result<Vec<u8>, String> {
        fn put<T: Serialize + ?Sized>(value: &T, out: Vec<u8>) -> Result<Vec<u8>, String> {
            postcard::to_extend(value, out).map_err(err)
        }
        if !self.core.exact_selected {
            return put(&0u8, tail);
        }
        match digests {
            Digests::Kept => {
                let (settled, rest) = self.core.exact_admissions.encode_parts().map_err(err)?;
                tail = put(&1u8, tail)?;
                tail = put(&settled.len().saturating_add(rest.len()), tail)?;
                parts.push(Piece::Part(tail.into()));
                parts.push(Piece::Part(settled));
                Ok(rest)
            }
            Digests::Taken => {
                let ((settled_len, settled_digest), rest) =
                    self.core.exact_admissions.digested_parts().map_err(err)?;
                tail = put(&1u8, tail)?;
                tail = put(
                    &settled_len.saturating_add(u64::try_from(rest.len()).unwrap_or(u64::MAX)),
                    tail,
                )?;
                parts.push(digested(&tail));
                parts.push(Piece::Digested(settled_len, settled_digest));
                Ok(rest)
            }
        }
    }

    /// Each sealed record's history, after every other stretch: as its parts, or as their
    /// lengths and digests.
    fn sealed_into(&self, digests: Digests, parts: &mut Vec<Piece>) -> Result<(), String> {
        for (route, entry) in &self.core.documents {
            if entry.sealed.is_empty() {
                continue;
            }
            match digests {
                Digests::Kept => {
                    let own = self.core.own.get(route);
                    let encoded = entry.encoded(route, || encode_document(route, entry, own))?;
                    if let Some(sealed) = &encoded.sealed {
                        parts.extend(sealed.iter().cloned().map(Piece::Part));
                    }
                }
                Digests::Taken => {
                    parts.extend(
                        entry
                            .sealed
                            .digested_parts(route)?
                            .into_iter()
                            .map(|(len, digest)| Piece::Digested(len, digest)),
                    );
                }
            }
        }
        Ok(())
    }

    /// The durable encoding as one `postcard` value, made whole each time:
    /// what [`Self::durable_parts`] must equal.
    #[cfg(test)]
    pub(crate) fn durable_whole(&self) -> Result<Vec<u8>, String> {
        let documents = self
            .core
            .documents
            .iter()
            .map(|(route, entry)| DurableDocument {
                route: route.clone(),
                bytes: {
                    let (base, since, _) = entry.doc.stored();
                    [&*base, &since.concat()].concat()
                },
                watermarks: entry
                    .channel
                    .last_seq_per_author
                    .iter()
                    .map(|(key, seq)| (*key.as_bytes(), *seq))
                    .collect(),
                encryption_required: entry.encryption_required,
                covered: entry.covered.clone(),
                covered_snapshot: entry
                    .covered_snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.0.bytes.clone()),
                own: self.core.own.get(route).cloned(),
            })
            .collect();
        let sealed: SealedHistory = self
            .core
            .documents
            .iter()
            .filter(|(_, entry)| !entry.sealed.is_empty())
            .map(|(route, entry)| (route.clone(), entry.sealed.to_vec()))
            .collect();
        if !sealed.is_empty() && !self.core.exact_selected {
            return Err("sealed record history requires exact admissions".into());
        }
        let exact = self
            .core
            .exact_selected
            .then(|| self.core.exact_admissions.encode())
            .transpose()
            .map_err(err)?;
        postcard::to_allocvec(&Durable {
            version: DURABLE_VERSION,
            workspace: self.core.workspace.clone(),
            owner: self.core.owner,
            workspace_bytes: self.core.workspace_doc.as_ref().map(|doc| {
                let (base, since, _) = doc.stored();
                [&*base, &since.concat()].concat()
            }),
            documents,
            receipts: (*self.core.receipts).clone(),
            certificates: self
                .core
                .certificates
                .values()
                .map(|cert| cert.as_ref().clone())
                .collect(),
            revocations: {
                let mut entries = self.core.revocations.iter().cloned().collect::<Vec<_>>();
                entries.sort_by_key(|entry| entry.revoked_device_public_key);
                entries
            },
            pending: self.core.pending.clone(),
            pending_bytes: self.core.pending_bytes,
            exact: exact.as_deref(),
            sealed,
        })
        .map_err(err)
    }

    /// Restore a peer from durable state produced by [`StatePeer::durable`].
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public API: callers hand over the pinned revocation list"
    )]
    pub fn restore(
        bytes: &[u8],
        workspace: &str,
        owner_device: DeviceCert,
        roster: Vec<DeviceCert>,
        revocations: RevocationList,
        now: SystemTime,
    ) -> Result<Self, String> {
        let durable = Durable::read(bytes, workspace, owner_device.user_public_key)?;
        let exact_admissions = durable
            .exact
            .map(ExactAdmissions::decode)
            .transpose()
            .map_err(|error| format!("exact admissions: {error}"))?;
        let canonical_workspace = uuid::Uuid::parse_str(workspace).map_err(err)?.to_string();
        let verified_certificates = backend::restore_certificate_roster(
            durable.certificates,
            roster,
            &owner_device,
            now_ns(now),
        )
        .map_err(err)?;
        let mut peer = Self {
            core: PeerCore {
                workspace: canonical_workspace,
                owner: owner_device.user_public_key,
                origin: owner_device.device_public_key,
                certificates: verified_certificates.into(),
                revocations: RevocationList::new(),
                receipts: Shared::default(),
                workspace_doc: None,
                documents: Shared::default(),
                pending: durable.pending,
                pending_bytes: durable.pending_bytes,
                exact_selected: exact_admissions.is_some(),
                exact_admissions: exact_admissions.unwrap_or_default(),
                native_cut_publisher: None,
                own: BTreeMap::new(),
            },
            surfaces: process_default().0,
            app_surfaces: process_default().1,
            missed: held::Missed::default(),
            gathering: super::compaction_set::Gathering::default(),
            workspace_host: false,
        };
        backend::validate_pending(&peer.core.pending, peer.core.pending_bytes).map_err(err)?;
        peer.core.revocations = backend::restore_revocation_union(
            durable.revocations,
            &revocations,
            &peer.core.certificates,
        )
        .map_err(err)?;
        peer.core.receipts = durable.receipts.into();
        peer.core.workspace_doc = durable
            .workspace_bytes
            .as_deref()
            .map(restored_workspace)
            .transpose()?;
        for document in durable.documents {
            check_governed_snapshot(
                peer.core.workspace_doc.as_deref(),
                &document.route,
                &document.covered,
                document.covered_snapshot.as_deref(),
            )?;
            // What the record is, as it was kept: only for the record it names.
            if let Some(own) = document.own.filter(|own| own.names(&document.route.record)) {
                peer.core.own.insert(document.route.clone(), own);
            }
            let channel = InboundState {
                last_seq_per_author: document
                    .watermarks
                    .into_iter()
                    .map(|(key, seq)| (Ed25519PublicKey::from_bytes(key), seq))
                    .collect(),
                ..InboundState::default()
            };
            peer.core.documents.insert(
                document.route,
                Document {
                    doc: tilo_automerge_isolate::load(&document.bytes)
                        .map(CachedDocument::from)
                        .map_err(err)?
                        .stored_as(&document.bytes)?,
                    channel,
                    encryption_required: document.encryption_required,
                    covered: document.covered,
                    covered_snapshot: document.covered_snapshot.map(AcceptedCheckpoint::new),
                    sealed: backend::SealedLog::default(),
                }
                .into(),
            );
        }
        peer.raise_floors_to_marks();
        for (route, ops) in durable.sealed {
            peer.restore_sealed(&route, ops)?;
        }
        if let Some(policy) = peer.validate_present_media_policy()? {
            let doc = peer
                .core
                .workspace_doc
                .as_deref()
                .ok_or("missing workspace")?
                .clone();
            // Durable identity metadata is not a substitute for matching the
            // governed certificate rows. Reuse the normal verified import and
            // retain historical certificates already validated during restore.
            peer.import_governed_certificates(&doc, now)?;
            // Workspace-first admission is durable even before its exact Chat
            // checkpoint arrives. Validate present bindings here; the media
            // projection still requires every active prerequisite to be ready.
            peer.validate_media_bindings(&policy, false)?;
        }
        peer.check_exact_within_floors()?;
        Ok(peer)
    }

    /// Every write of an author a compaction folded is decided: the author's floor is at least
    /// its mark. A record whose floor fell below a mark took no write of that author again,
    /// each one numbered under writes it decided already; it comes back with its floor at the
    /// mark.
    fn raise_floors_to_marks(&mut self) {
        let marks: Vec<(Route, [u8; 32], u64)> = self
            .core
            .exact_admissions
            .marks()
            .filter(|(writer, _)| writer.workspace == self.core.workspace)
            .map(|(writer, mark)| {
                (
                    Route {
                        app: writer.app.clone(),
                        record: writer.record.clone(),
                    },
                    writer.author,
                    mark,
                )
            })
            .collect();
        for (route, author, mark) in marks {
            if let Some(document) = self.core.documents.get_mut(&route) {
                let floor = document
                    .channel
                    .last_seq_per_author
                    .entry(Ed25519PublicKey::from_bytes(author))
                    .or_default();
                *floor = (*floor).max(mark);
            }
        }
    }

    /// Every exact entry and every compaction mark lies within its route's
    /// watermark.
    fn check_exact_within_floors(&self) -> Result<(), String> {
        let index = &self.core.exact_admissions;
        let keys: Vec<_> = index
            .iter()
            .map(|(key, _)| key.clone())
            .chain(index.marks().map(|(writer, mark)| writer.at(mark)))
            .collect();
        for key in &keys {
            let floor = self
                .core
                .documents
                .get(&Route {
                    app: key.app.clone(),
                    record: key.record.clone(),
                })
                .and_then(|document| {
                    document
                        .channel
                        .last_seq_per_author
                        .get(&Ed25519PublicKey::from_bytes(key.author))
                })
                .copied();
            if key.workspace != self.core.workspace
                || floor.is_none_or(|floor| key.sequence > floor)
            {
                return Err("exact admission evidence exceeds restored route/floor".into());
            }
        }
        Ok(())
    }
}

/// A record's part of the durable encoding: its `DurableDocument` in parts —
/// the route and the document's length; its stored base, the same allocation
/// while the base lasts; the runs of changes since the base, each the same
/// allocation while it lasts; the fields after the document — and its sealed
/// history, when it holds one.
fn encode_document(
    route: &Route,
    entry: &Document,
    own: Option<&collab_types::OwnResource>,
) -> Result<backend::EncodedDocument, String> {
    fn put<T: Serialize + ?Sized>(value: &T, out: Vec<u8>) -> Result<Vec<u8>, String> {
        postcard::to_extend(value, out).map_err(err)
    }
    let (base, since, _) = entry.doc.stored();
    let since_len = since.iter().map(|run| run.len()).sum::<usize>();
    let head = put(&(route, base.len().saturating_add(since_len)), Vec::new())?;
    let rest = put(
        &(
            entry
                .channel
                .last_seq_per_author
                .iter()
                .map(|(key, seq)| (*key.as_bytes(), *seq))
                .collect::<Vec<_>>(),
            entry.encryption_required,
            &entry.covered,
            entry
                .covered_snapshot
                .as_ref()
                .map(|snapshot| snapshot.0.bytes.as_slice()),
            own,
        ),
        Vec::new(),
    )?;
    let sealed = if entry.sealed.is_empty() {
        None
    } else {
        Some(entry.sealed.encode_parts(route)?)
    };
    Ok(backend::EncodedDocument {
        route: route.clone(),
        document: vec![
            Piece::Part(head.into()),
            Piece::Part(base),
            Piece::Since(since),
            Piece::Part(rest.into()),
        ],
        sealed,
    })
}

/// The length and digest of `bytes`, as a piece a durable hash folds in without keeping them.
fn digested(bytes: &[u8]) -> Piece {
    let (len, digest) = length_and_digest(bytes);
    Piece::Digested(len, digest)
}

/// The length of `bytes` and its digest.
fn length_and_digest(bytes: &[u8]) -> super::StretchDigest {
    (
        u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        *blake3::hash(bytes).as_bytes(),
    )
}

/// [`encode_document`] as the lengths and digests of its parts, taken without keeping the
/// document's bytes: what a session that stores nothing folds into its durable hash.
fn digest_document(
    route: &Route,
    entry: &Document,
    own: Option<&collab_types::OwnResource>,
) -> Result<Vec<super::StretchDigest>, String> {
    fn put<T: Serialize + ?Sized>(value: &T, out: Vec<u8>) -> Result<Vec<u8>, String> {
        postcard::to_extend(value, out).map_err(err)
    }
    let layout = entry.doc.stored_layout();
    let head = put(
        &(route, layout.base_len.saturating_add(layout.since_len)),
        Vec::new(),
    )?;
    let rest = put(
        &(
            entry
                .channel
                .last_seq_per_author
                .iter()
                .map(|(key, seq)| (*key.as_bytes(), *seq))
                .collect::<Vec<_>>(),
            entry.encryption_required,
            &entry.covered,
            entry
                .covered_snapshot
                .as_ref()
                .map(|snapshot| snapshot.0.bytes.as_slice()),
            own,
        ),
        Vec::new(),
    )?;
    Ok(vec![
        length_and_digest(&head),
        (
            u64::try_from(layout.base_len).unwrap_or(u64::MAX),
            layout.base_digest,
        ),
        (
            u64::try_from(layout.since_len).unwrap_or(u64::MAX),
            layout.since_digest,
        ),
        length_and_digest(&rest),
    ])
}

/// The workspace document a peer kept, loaded in isolation so it stays in a
/// worker and the first inbound change does not pay a full rebuild.
fn restored_workspace(bytes: &[u8]) -> Result<CachedDocument, String> {
    tilo_automerge_isolate::load(bytes)
        .map(CachedDocument::from)
        .map_err(err)
        .and_then(|doc| doc.stored_as(bytes))
}
