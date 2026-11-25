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
use automerge::transaction::Transactable as _;

/// The durable layout this build writes and reads. A change to what is
/// stored bumps this: a build refuses state of another version by name
/// rather than reading it as this one's, except the versions since
/// [`RELABELLED`], which it brings up ([`brought_up`]).
pub const DURABLE_VERSION: u32 = 18;

/// The version before [`DURABLE_VERSION`]: the same layout, whose waiting writes may not be a
/// message of a kind this build appends ([`NeighbourMsg::DocumentContentKept`] and
/// [`NeighbourMsg::DocumentContentReleased`]), so a build that reads neither refuses a state that
/// carries one rather than reading its writes wrong ([`brought_up`]).
pub(super) const PREVIOUS: u32 = 17;

/// The oldest version before [`PREVIOUS`] that has this build's layout: version 16 differs only
/// in the content messages a waiting write may be, version 15 only in what a certificate it keeps
/// may name, and version 14 only in what a governance row may say of a device the workspace
/// removed, so each comes up relabelled ([`brought_up`]).
pub(super) const RELABELLED: u32 = 14;

/// The newest version before [`RELABELLED`] that this build brings up ([`brought_up`]).
pub(super) const BROUGHT_UP: u32 = 13;

/// Whether this build brings up a state of durable layout `version` ([`brought_up`]).
fn brings_up(version: u32) -> bool {
    (RELABELLED..=PREVIOUS).contains(&version) || version == BROUGHT_UP
}

/// `bytes`, a durable state, in this build's layout: versions 17 ([`PREVIOUS`]) to 14
/// ([`RELABELLED`]) relabelled, version 13 ([`brings_up`]) brought up to it, any other as it is,
/// for [`Durable::read`] to read or refuse.
///
/// Version 17 differs from this one only in what a write waiting to be sent may be: this build
/// appends the word that pushed content is kept and that a member lets go of it, which version
/// 17's builds refuse, and a message version 17 wrote reads the same here. Version 16 differs
/// from version 17 the same way, by the messages content is asked for and answered with many at
/// a time. Version 15 differs
/// from version 16 only in what a certificate it keeps may name: this build's may name a placed
/// device's records or a placement grant, under binary markers version 15's builds refuse, and a
/// certificate version 15 wrote reads the same here. Version 14 differs from version 15 only in
/// what a governance row may say of a device the workspace removed: version 15 marks the row,
/// and there is no other byte of itself to change.
///
/// Version 13 kept the writes it held for a prerequisite and each encrypted record's sealed
/// writes as signed ops whose claims name no call. This build's claims write the call, which the
/// op's signature covers, so those ops cannot be written again with a signature that still
/// verifies: they are dropped as the state comes up, and their workspace's host serves them
/// again when the device subscribes, as it does after any restart. What the state holds besides
/// them comes up as it was.
pub(super) fn brought_up(bytes: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>, String> {
    let (version, rest) = postcard::take_from_bytes::<u32>(bytes).map_err(err)?;
    if (RELABELLED..=PREVIOUS).contains(&version) {
        // This build's layout with its version relabelled: the messages, a
        // placed device's certificate and the mark a removed device's row
        // carries need no other change to read.
        let mut kept = postcard::to_allocvec(&DURABLE_VERSION).map_err(err)?;
        kept.extend_from_slice(rest);
        return Ok(kept.into());
    }
    if !brings_up(version) {
        return Ok(bytes.into());
    }
    let durable = before_call(bytes).map_err(|error| {
        format!(
            "this state is of durable layout {version} and does not read as it: {error}. Move \
             the state directory aside, or start with an empty one"
        )
    })?;
    postcard::to_allocvec(&Durable {
        version: DURABLE_VERSION,
        workspace: durable.workspace,
        owner: durable.owner,
        workspace_bytes: durable.workspace_bytes,
        documents: durable.documents,
        receipts: durable.receipts,
        certificates: durable.certificates,
        revocations: durable.revocations,
        pending: VecDeque::new(),
        pending_bytes: 0,
        exact: durable.exact,
        sealed: Vec::new(),
    })
    .map(Into::into)
    .map_err(err)
}

/// A signed op as version 13 wrote it: its claims name each run's changes, action, units and
/// invoker text, and no call. What the state held of it that is signed is dropped as it comes up
/// ([`brought_up`]).
#[derive(Deserialize)]
#[expect(
    dead_code,
    reason = "a version 13 op is read to find where the state's dropped writes end"
)]
struct SignedOpBeforeCall {
    #[serde(with = "serde_bytes")]
    payload: Vec<u8>,
    artifact: collab_types::ArtifactId,
    author: collab_types::Ed25519PublicKey,
    attestation_id: collab_types::AttestationId,
    kind: collab_types::OpKind,
    timestamp: SystemTime,
    seq: u64,
    encryption: Option<collab_types::EncryptionMetadata>,
    claims: Vec<RunBeforeCall>,
    surface: Option<[u8; 32]>,
    governance: Vec<[u8; 4]>,
    signature: collab_types::Signature,
}

#[derive(Deserialize)]
#[expect(
    dead_code,
    reason = "a version 13 claim run is read to find where the state's dropped writes end"
)]
struct RunBeforeCall {
    changes: Vec<[u8; 32]>,
    action: collab_types::ClaimedAction,
    units: Vec<collab_types::UnitPath>,
    invoker: Option<String>,
}

/// One document write held for a prerequisite, as version 13 kept it.
#[derive(Deserialize)]
#[expect(
    dead_code,
    reason = "a version 13 held write is read to find where the state's dropped writes end"
)]
struct DocumentWriteBeforeCall {
    peer: neighbour_wire::PeerId,
    app_id: String,
    cluster_id: String,
    document_id: String,
    envelope: SignedOpBeforeCall,
    readers: Option<String>,
}

/// A version 13 state, with what this build reads of it and nothing of what it drops
/// ([`brought_up`]).
fn before_call(bytes: &[u8]) -> Result<Durable<'_>, postcard::Error> {
    type Head = (u32, String, [u8; 32], Option<Vec<u8>>, usize);
    let ((_, workspace, owner, workspace_bytes, count), mut rest) =
        postcard::take_from_bytes::<Head>(bytes)?;
    let mut documents = Vec::new();
    for _ in 0..count {
        let (document, after) = postcard::take_from_bytes::<DurableDocument>(rest)?;
        documents.push(document);
        rest = after;
    }
    let ((receipts, certificates, revocations, held), mut rest) = postcard::take_from_bytes::<(
        BTreeMap<String, Vec<u8>>,
        Vec<DeviceCert>,
        Vec<tilo_user_identity::RevocationEntry>,
        usize,
    )>(rest)?;
    // The held writes are read only to find where they end: their signed bytes are dropped.
    for _ in 0..held {
        let (_, after) = postcard::take_from_bytes::<[u8; 32]>(rest)?;
        let after = after_before_call(after)?;
        let (_, after) = postcard::take_from_bytes::<bool>(after)?;
        rest = after;
    }
    let ((_, exact), rest) = postcard::take_from_bytes::<(usize, Option<&[u8]>)>(rest)?;
    // Each sealed record's writes: read to find the end of the state, and dropped with the held
    // writes.
    let (_, rest) = postcard::take_from_bytes::<Vec<(Route, Vec<SignedOpBeforeCall>)>>(rest)?;
    if !rest.is_empty() {
        return Err(postcard::Error::DeserializeBadEncoding);
    }
    Ok(Durable {
        version: DURABLE_VERSION,
        workspace,
        owner,
        workspace_bytes,
        documents,
        receipts,
        certificates,
        revocations,
        pending: VecDeque::new(),
        pending_bytes: 0,
        exact,
        sealed: Vec::new(),
    })
}

/// The bytes after the one message at the start of `bytes` as version 13 kept it: a document
/// write read in version 13's claim layout, any other message as it is.
fn after_before_call(bytes: &[u8]) -> Result<&[u8], postcard::Error> {
    let Some(fields) = bytes.strip_prefix(&[NeighbourMsg::APP_DOCUMENT_TAG]) else {
        return postcard::take_from_bytes::<NeighbourMsg>(bytes).map(|(_, rest)| rest);
    };
    postcard::take_from_bytes::<DocumentWriteBeforeCall>(fields).map(|(_, rest)| rest)
}

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
    /// The durable state `bytes` hold, of `workspace`.
    pub(super) fn read(bytes: &'a [u8], workspace: &str) -> Result<Self, String> {
        let (version, _) = postcard::take_from_bytes::<u32>(bytes).map_err(err)?;
        if version != DURABLE_VERSION {
            return Err(if version < DURABLE_VERSION {
                format!(
                    "this state is from an older format (version {version}, and this build \
                     reads version {DURABLE_VERSION}): move the state directory aside, or start \
                     with an empty one"
                )
            } else {
                format!(
                    "this state is from a newer format (version {version}, and this build \
                     reads version {DURABLE_VERSION}): run a newer build, or move the state \
                     directory aside and start with an empty one"
                )
            });
        }
        // Bytes of this version that do not decode are of another layout that
        // called itself this version: said plainly, never as a decode's words
        // alone.
        let (durable, tail): (Self, _) = postcard::take_from_bytes(bytes).map_err(|error| {
            format!(
                "this state cannot be read by this build (it may be from another format): \
                 {error}. Move the state directory aside, or start with an empty one"
            )
        })?;
        if !tail.is_empty() {
            return Err("trailing bytes after durable state".into());
        }
        if !durable.sealed.is_empty() && durable.exact.is_none() {
            return Err("sealed record history requires exact admissions".into());
        }
        if durable.workspace != workspace {
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

/// Whether [`StatePeer::around_documents`] keeps the encoding's parts or only their digests.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Digests {
    /// Keep the parts: a store writes them.
    Kept,
    /// Keep only lengths and digests: nothing here is stored, so a session pays for a hash, not
    /// for a second copy of every record beside the replica it holds.
    Taken,
}

/// Each document of `documents` decoded, in their order: a few at a time, as a start with
/// many documents spends its time here and one decoded at a time made it wait for every part
/// in turn.
fn decode_documents(documents: &[DurableDocument]) -> Vec<Result<CachedDocument, String>> {
    /// Documents decoded at once: enough to keep the cores and the disks busy without
    /// holding a workspace's worth of decoders.
    #[cfg(not(target_arch = "wasm32"))]
    const AT_ONCE: usize = 4;
    // A wasm build has no threads to decode on: it decodes in this one.
    #[cfg(target_arch = "wasm32")]
    let workers = 1;
    #[cfg(not(target_arch = "wasm32"))]
    let workers = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .clamp(1, AT_ONCE)
        .min(documents.len().max(1));
    if workers == 1 {
        return documents.iter().map(decode_document).collect();
    }
    #[cfg(target_arch = "wasm32")]
    {
        documents.iter().map(decode_document).collect()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let ready: std::sync::Mutex<Vec<Option<Result<CachedDocument, String>>>> =
            std::sync::Mutex::new(documents.iter().map(|_| None).collect());
        // The helpers decode for this thread's caller: a workspace restoring many documents
        // takes its turn at the shared workers as one caller, not as one per helper.
        let caller = tilo_automerge_isolate::Caller::current();
        std::thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    caller.enter(|| {
                        loop {
                            let at = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(document) = documents.get(at) else {
                                break;
                            };
                            let decoded = decode_document(document);
                            let mut ready = ready
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if let Some(held) = ready.get_mut(at) {
                                *held = Some(decoded);
                            }
                        }
                    });
                });
            }
        });
        let ready = ready
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ready
            .into_iter()
            .map(|held| held.unwrap_or_else(|| Err("a document was not decoded".to_owned())))
            .collect()
    }
}

fn decode_document(document: &DurableDocument) -> Result<CachedDocument, String> {
    tilo_automerge_isolate::load(&document.bytes)
        .map(CachedDocument::from)
        .map_err(err)
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
            .all(|head| accepted.doc.has_change(head));
        Ok(Some((receipt, current)))
    }
    /// Whether the durable receipt for the record covers its current heads.
    #[cfg(test)]
    pub fn durable_covers_current(&self, app: &str, record: &str) -> Result<bool, String> {
        Ok(self
            .durable_coverage_with_current(app, record)?
            .is_some_and(|(_, current)| current))
    }
    /// A digest of [`Self::durable`]: of the length and digest of each of its stretches
    /// ([`Piece`]) in order, with its documents folded into a Merkle tree in their place
    /// ([`super::documents`]). A document's leaf digests its stretches, and those of its sealed
    /// history; only its lengths and digests are kept, per state of the document, because a
    /// session that stores nothing would otherwise hold a second copy of every document beside
    /// the replica it holds. A commit takes this digest for its receipt, and pays for the documents
    /// it changed: the tree takes again only their leaves.
    pub fn durable_hash(&self) -> Result<[u8; 32], String> {
        let (before, after) = self.around_documents(Digests::Taken)?;
        let root = self.core.documents.root(Self::leaf)?;
        fold_durable_hash(&before, root, &after)
    }

    /// The leaf of `route`'s document `entry` in the documents' tree: the lengths and digests of
    /// its part of the durable encoding and of its sealed history, made once per state of it.
    fn leaf(route: &Route, entry: &backend::StoredDocument) -> Result<[u8; 32], String> {
        let document = entry.digests(|| digest_document(route, entry))?;
        let sealed = entry.sealed_entry_digests(route)?;
        Ok(leaf_of(document, &sealed))
    }

    /// [`Self::durable_hash`] over the parts themselves, as a store would write them: the same
    /// digests, taken from the bytes, and every document's leaf taken again. What the digest path
    /// must answer.
    #[cfg(test)]
    pub(crate) fn durable_hash_of_kept_parts(&self) -> Result<[u8; 32], String> {
        fn stretch(piece: &Piece) -> super::StretchDigest {
            match piece {
                Piece::Part(part) => length_and_digest(part),
                Piece::Since(runs) => {
                    let mut digest = blake3::Hasher::new();
                    for run in runs {
                        digest.update(run);
                    }
                    (
                        u64::try_from(runs.iter().map(|run| run.len()).sum::<usize>())
                            .unwrap_or(u64::MAX),
                        *digest.finalize().as_bytes(),
                    )
                }
                Piece::Digested(len, digest) => (*len, *digest),
            }
        }
        let (before, after) = self.around_documents(Digests::Kept)?;
        let mut leaves = Vec::with_capacity(self.core.documents.len());
        for (route, entry) in &self.core.documents {
            let encoded = entry.encoded(route, || encode_document(route, entry))?;
            let document: Vec<_> = encoded.document.iter().map(stretch).collect();
            let sealed: Vec<_> = encoded
                .sealed
                .iter()
                .flatten()
                .map(|part| length_and_digest(part))
                .collect();
            leaves.push((route.clone(), leaf_of(&document, &sealed)));
        }
        let root = super::documents::root_of(leaves)?;
        let digested = |pieces: &[Piece]| -> Vec<Piece> {
            pieces
                .iter()
                .map(|piece| {
                    let (len, digest) = stretch(piece);
                    Piece::Digested(len, digest)
                })
                .collect()
        };
        fold_durable_hash(&digested(&before), root, &digested(&after))
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
        Ok(self.durable_layout()?.to_vec())
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
    pub fn durable_parts(&self) -> Result<Vec<std::sync::Arc<[u8]>>, String> {
        Ok(self.durable_layout()?.parts().cloned().collect())
    }

    /// [`Self::durable`] as its stretches before the documents, each
    /// document's, and those after them.
    ///
    /// A workspace's history is most of its state and hardly changes per
    /// write, yet re-encoding all of it made every change cost the whole
    /// history. So each document's part is encoded once per state of the
    /// document, made again only for the documents changed since it was last
    /// made, and the exact admission index's settled entries once per
    /// settling: a part nothing changed since an earlier call is the same
    /// allocation as then, the documents' parts are a map whose copies share
    /// what neither changed, and whoever stores the parts apart rewrites only
    /// what changed.
    pub(crate) fn durable_layout(&self) -> Result<DurableParts, String> {
        let (before, after) = self.around_documents(Digests::Kept)?;
        let documents = self.core.documents.parts(|route, entry| {
            let encoded = entry.encoded(route, || encode_document(route, entry))?;
            Ok(DocumentParts {
                document: flatten(&encoded.document),
                sealed: encoded
                    .sealed
                    .as_ref()
                    .map(|sealed| sealed.as_slice().into()),
            })
        })?;
        Ok(DurableParts {
            head: flatten(&before),
            documents,
            tail: flatten(&after),
        })
    }

    /// The stretches of [`Self::durable_parts`] before its documents, and those after them up to
    /// their sealed histories.
    fn around_documents(&self, digests: Digests) -> Result<(Vec<Piece>, Vec<Piece>), String> {
        fn put<T: Serialize + ?Sized>(value: &T, out: Vec<u8>) -> Result<Vec<u8>, String> {
            postcard::to_extend(value, out).map_err(err)
        }
        let sealed = self.core.documents.sealed_count();
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
        let mut after = Vec::new();
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
        tail = self.exact_into(tail, digests, &mut after)?;
        tail = put(&sealed, tail)?;
        after.push(match digests {
            Digests::Kept => Piece::Part(tail.into()),
            Digests::Taken => digested(&tail),
        });
        Ok((parts, after))
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
            })
            .collect();
        let sealed: SealedHistory = self
            .core
            .documents
            .iter()
            .filter(|(_, entry)| entry.stores_sealed())
            .map(|(route, entry)| (route.clone(), entry.sealed_entry_writes()))
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
    /// The workspace's owner is the one its governance names, which the
    /// durable state carries; `owner_device` is the device this replica was
    /// made on or joined through ([`StatePeer::from_join`]).
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
        let bytes = brought_up(bytes)?;
        let durable = Durable::read(&bytes, workspace)?;
        let owner = durable.owner;
        let exact_admissions = durable
            .exact
            .map(ExactAdmissions::decode)
            .transpose()
            .map_err(|error| format!("exact admissions: {error}"))?;
        let verified_certificates = backend::restore_certificate_roster(
            durable.certificates,
            roster,
            &owner_device,
            now_ns(now),
        )
        .map_err(err)?;
        let mut peer = Self {
            core: PeerCore {
                workspace: uuid::Uuid::parse_str(workspace).map_err(err)?.to_string(),
                owner,
                origin: owner_device.device_public_key,
                certificates: verified_certificates.into(),
                // A placed device proves itself again each time it links: its certificate is
                // short-lived and no part of the workspace's kept state.
                placed_devices: Shared::default(),
                revocations: RevocationList::new(),
                receipts: Shared::default(),
                workspace_doc: None,
                documents: super::documents::Documents::default(),
                pending: durable.pending,
                pending_bytes: durable.pending_bytes,
                exact_selected: exact_admissions.is_some(),
                exact_admissions: exact_admissions.unwrap_or_default(),
            },
            surfaces: process_default().0,
            app_surfaces: process_default().1,
            missed: held::Missed::default(),
            gathering: super::compaction_set::Gathering::default(),
            workspace_host: false,
            own_device: None,
            governance_now: None,
            copied: std::collections::BTreeMap::new(),
            action_stamp: None,
        };
        backend::validate_pending(&peer.core.pending, peer.core.pending_bytes).map_err(err)?;
        peer.core.revocations = backend::restore_revocation_union(
            durable.revocations,
            &revocations,
            &peer.core.certificates,
        )
        .map_err(err)?;
        peer.core.receipts = durable.receipts.into();
        peer.core.workspace_doc =
            restored_governance(durable.workspace_bytes.as_deref(), workspace, owner)?;
        // The workspace's documents are decoded side by side: a workspace keeps a chain of
        // documents, each with its history, and one decoded at a time made a start wait for
        // every part in turn.
        let decoded = decode_documents(&durable.documents);
        for (document, decoded) in durable.documents.into_iter().zip(decoded) {
            check_governed_snapshot(
                peer.core.workspace_doc.as_deref(),
                &document.route,
                &document.covered,
                document.covered_snapshot.as_deref(),
            )?;
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
                    doc: decoded?.stored_as(&document.bytes)?,
                    channel,
                    encryption_required: document.encryption_required,
                    covered: document.covered,
                    covered_snapshot: document.covered_snapshot.map(AcceptedCheckpoint::new),
                    sealed: backend::SealedLog::default(),
                    checkpoint_writes: std::sync::Arc::default(),
                    quarantine: None,
                }
                .into(),
            );
        }
        peer.raise_floors_to_marks();
        // A record whose stored history its checks refuse is set aside, never read: the rest of
        // the workspace opens, and the record asks the workspace's devices for its history again
        // ([`super::quarantine`]).
        for (route, ops) in durable.sealed {
            if let Err(refused) = super::sealed_history::restore(&mut peer.core, &route, ops) {
                peer.quarantine(&route, refused)?;
            }
        }
        peer.check_exact_within_floors()?;
        Ok(peer)
    }

    /// Every write of an author a compaction folded is decided: the author's floor is at least
    /// its mark. A record whose floor fell below a mark took no write of that author again,
    /// each one numbered under writes it decided already; it comes back with its floor at the
    /// mark.
    pub(super) fn raise_floors_to_marks(&mut self) {
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
    /// watermark. A sealed record whose evidence does not is set aside as one
    /// whose stored history is refused ([`super::quarantine`]); evidence
    /// beyond any other record, or naming no record held, refuses the state.
    fn check_exact_within_floors(&mut self) -> Result<(), String> {
        const BEYOND: &str = "exact admission evidence exceeds restored route/floor";
        let index = &self.core.exact_admissions;
        let keys: Vec<_> = index
            .iter()
            .map(|(key, _)| key.clone())
            .chain(index.marks().map(|(writer, mark)| writer.at(mark)))
            .collect();
        let mut beyond = std::collections::BTreeSet::new();
        for key in &keys {
            let route = Route {
                app: key.app.clone(),
                record: key.record.clone(),
            };
            let document = self.core.documents.get(&route);
            if key.workspace != self.core.workspace || document.is_none() {
                return Err(BEYOND.into());
            }
            let floor = document
                .and_then(|document| {
                    document
                        .channel
                        .last_seq_per_author
                        .get(&Ed25519PublicKey::from_bytes(key.author))
                })
                .copied();
            if floor.is_none_or(|floor| key.sequence > floor) {
                if !document.is_some_and(|document| document.encryption_required) {
                    return Err(BEYOND.into());
                }
                beyond.insert(route);
            }
        }
        for route in beyond {
            self.set_aside(&route, BEYOND);
        }
        Ok(())
    }
}

/// A record's part of the durable encoding: its `DurableDocument` in parts —
/// the route and the document's length; its stored base, the same allocation
/// while the base lasts; the runs of changes since the base, each the same
/// allocation while it lasts; the fields after the document — and its sealed
/// history, when it holds one.
fn encode_document(route: &Route, entry: &Document) -> Result<backend::EncodedDocument, String> {
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
        ),
        Vec::new(),
    )?;
    let sealed = entry.sealed_entry(route)?;
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

/// The parts `pieces` hold, in order.
fn flatten(pieces: &[Piece]) -> std::sync::Arc<[std::sync::Arc<[u8]>]> {
    pieces
        .iter()
        .flat_map(|piece| match piece {
            Piece::Part(part) => vec![std::sync::Arc::clone(part)],
            Piece::Since(since) => since.clone(),
            Piece::Digested(..) => Vec::new(),
        })
        .collect()
}

/// The fields of `Durable` after its documents and before its sealed histories.
type TailFields<'a> = (
    BTreeMap<String, Vec<u8>>,
    Vec<DeviceCert>,
    Vec<tilo_user_identity::RevocationEntry>,
    VecDeque<([u8; 32], NeighbourMsg, bool)>,
    usize,
    Option<&'a [u8]>,
);

/// One document's part of a durable encoding: its `DurableDocument`, and its
/// entry of the sealed histories after every document, when it has one.
#[derive(Clone, Debug)]
pub(crate) struct DocumentParts {
    pub(crate) document: std::sync::Arc<[std::sync::Arc<[u8]>]>,
    pub(crate) sealed: Option<std::sync::Arc<[std::sync::Arc<[u8]>]>>,
}

impl DocumentParts {
    /// Whether `a` and `b` are one encoding no copy changed.
    pub(crate) fn same(a: &Self, b: &Self) -> bool {
        std::sync::Arc::ptr_eq(&a.document, &b.document)
            && match (&a.sealed, &b.sealed) {
                (None, None) => true,
                (Some(a), Some(b)) => std::sync::Arc::ptr_eq(a, b),
                _ => false,
            }
    }

    /// Whether `a` and `b` are the same bytes, however they are split.
    fn same_bytes(a: &Self, b: &Self) -> bool {
        same_bytes(&a.document, &b.document)
            && match (&a.sealed, &b.sealed) {
                (None, None) => true,
                (Some(a), Some(b)) => same_bytes(a, b),
                _ => false,
            }
    }
}

/// A replica's durable encoding ([`StatePeer::durable`]) as its stretches: those
/// before its documents, each document's, and those after them. Concatenated
/// in that order, each document's sealed history last, they are its bytes.
/// The documents are a map whose copies share what neither changed, so two
/// encodings tell what changed between them by what they no longer share.
#[derive(Clone, Debug, Default)]
pub(crate) struct DurableParts {
    pub(crate) head: std::sync::Arc<[std::sync::Arc<[u8]>]>,
    pub(crate) documents: crate::ordered::Ordered<Route, DocumentParts>,
    pub(crate) tail: std::sync::Arc<[std::sync::Arc<[u8]>]>,
}

impl DurableParts {
    /// The parts, in the order of the bytes.
    pub(crate) fn parts(&self) -> impl Iterator<Item = &std::sync::Arc<[u8]>> + '_ {
        self.head
            .iter()
            .chain(
                self.documents
                    .values()
                    .flat_map(|parts| parts.document.iter()),
            )
            .chain(self.tail.iter())
            .chain(
                self.documents
                    .values()
                    .flat_map(|parts| parts.sealed.iter().flat_map(|sealed| sealed.iter())),
            )
    }

    /// How many bytes they hold.
    pub(crate) fn len(&self) -> usize {
        self.parts().map(|part| part.len()).sum()
    }

    /// The bytes whole.
    pub(crate) fn to_vec(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.len());
        for part in self.parts() {
            bytes.extend_from_slice(part);
        }
        bytes
    }

    /// `bytes` as one stretch before no documents.
    pub(crate) fn whole(bytes: &[u8]) -> Self {
        Self {
            head: std::sync::Arc::from([std::sync::Arc::from(bytes)]),
            ..Self::default()
        }
    }

    /// The durable encoding `bytes` as its stretches, each one part.
    ///
    /// # Errors
    /// The bytes are not a durable encoding this build reads.
    pub(crate) fn read(bytes: &[u8]) -> Result<Self, String> {
        let bytes = brought_up(bytes)?;
        Self::read_today(&bytes)
    }

    /// [`Self::read`] of a state in this build's layout, as [`brought_up`] left it.
    fn read_today(bytes: &[u8]) -> Result<Self, String> {
        fn take<'a, T: Deserialize<'a>>(rest: &mut &'a [u8]) -> Result<T, String> {
            let (value, after) = postcard::take_from_bytes(rest).map_err(|error| {
                format!(
                    "this state cannot be read by this build (it may be from another \
                     format): {error}. Move the state directory aside, or start with an \
                     empty one"
                )
            })?;
            *rest = after;
            Ok(value)
        }
        let at = |rest: &[u8]| bytes.len().saturating_sub(rest.len());
        let part = |from: usize, to: usize| -> std::sync::Arc<[u8]> {
            bytes.get(from..to).unwrap_or_default().into()
        };
        let mut rest = bytes;
        let version: u32 = take(&mut rest)?;
        if version != DURABLE_VERSION {
            return Err(format!(
                "this state is of durable layout {version}, and this build reads layout \
                 {DURABLE_VERSION}. Move the state directory aside, or start with an empty one"
            ));
        }
        let (_workspace, _owner, _governance): (String, [u8; 32], Option<&[u8]>) = take(&mut rest)?;
        let count: usize = take(&mut rest)?;
        let head_end = at(rest);
        let mut documents = crate::ordered::Ordered::default();
        for _ in 0..count {
            let from = at(rest);
            let route = take::<DurableDocument>(&mut rest)?.route;
            documents.insert(
                route,
                DocumentParts {
                    document: std::sync::Arc::from([part(from, at(rest))]),
                    sealed: None,
                },
            );
        }
        let tail_from = at(rest);
        let _fields: TailFields<'_> = take(&mut rest)?;
        let sealed: usize = take(&mut rest)?;
        let tail = part(tail_from, at(rest));
        for _ in 0..sealed {
            let from = at(rest);
            let (route, _): (Route, Vec<collab_types::SignedOp>) = take(&mut rest)?;
            let held: &mut DocumentParts = documents
                .get_mut(&route)
                .ok_or("a sealed history names a document the state does not hold")?;
            held.sealed = Some(std::sync::Arc::from([part(from, at(rest))]));
        }
        if !rest.is_empty() {
            return Err("trailing bytes after durable state".into());
        }
        Ok(Self {
            head: std::sync::Arc::from([part(0, head_end)]),
            documents,
            tail: std::sync::Arc::from([tail]),
        })
    }
}

/// The same bytes, however they are split: what neither copy changed is not read.
impl PartialEq for DurableParts {
    fn eq(&self, other: &Self) -> bool {
        same_bytes(&self.head, &other.head)
            && same_bytes(&self.tail, &other.tail)
            && self.documents.len() == other.documents.len()
            && crate::ordered::Ordered::changes(&self.documents, &other.documents, |a, b| {
                DocumentParts::same(a, b) || DocumentParts::same_bytes(a, b)
            })
            .is_empty()
    }
}

/// Whether the parts `a` and `b` hold the same bytes, however they are split:
/// parts that are one allocation are not read.
pub(crate) fn same_bytes(a: &[std::sync::Arc<[u8]>], b: &[std::sync::Arc<[u8]>]) -> bool {
    if a.len() == b.len() && a.iter().zip(b).all(|(a, b)| std::sync::Arc::ptr_eq(a, b)) {
        return true;
    }
    let (mut ours, mut theirs) = (a.iter().map(|part| &**part), b.iter().map(|part| &**part));
    let (mut a, mut b): (&[u8], &[u8]) = (&[], &[]);
    loop {
        while a.is_empty() {
            match ours.next() {
                Some(next) => a = next,
                None => return b.is_empty() && theirs.all(<[u8]>::is_empty),
            }
        }
        while b.is_empty() {
            match theirs.next() {
                Some(next) => b = next,
                None => return false,
            }
        }
        let n = a.len().min(b.len());
        let (Some((a_now, a_next)), Some((b_now, b_next))) =
            (a.split_at_checked(n), b.split_at_checked(n))
        else {
            return false;
        };
        if a_now != b_now {
            return false;
        }
        (a, b) = (a_next, b_next);
    }
}

/// A document's leaf in the documents' tree: the lengths and digests of its part of the durable
/// encoding, then those of its sealed history.
fn leaf_of(document: &[super::StretchDigest], sealed: &[super::StretchDigest]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("Tilo durable document v1");
    for stretches in [document, sealed] {
        hasher.update(&(stretches.len() as u64).to_le_bytes());
        for (len, digest) in stretches {
            hasher.update(&len.to_le_bytes());
            hasher.update(digest);
        }
    }
    *hasher.finalize().as_bytes()
}

/// The durable hash of the stretches `before` the documents, the documents' tree's `root`, and the
/// stretches `after` them.
fn fold_durable_hash(
    before: &[Piece],
    root: [u8; 32],
    after: &[Piece],
) -> Result<[u8; 32], String> {
    fn fold(hasher: &mut blake3::Hasher, pieces: &[Piece]) -> Result<(), String> {
        for piece in pieces {
            let Piece::Digested(len, digest) = piece else {
                return Err("a durable hash was taken from parts".to_owned());
            };
            hasher.update(&len.to_le_bytes());
            hasher.update(digest);
        }
        Ok(())
    }
    let mut hasher = blake3::Hasher::new_derive_key("Tilo durable state v2");
    fold(&mut hasher, before)?;
    hasher.update(&root);
    fold(&mut hasher, after)?;
    Ok(*hasher.finalize().as_bytes())
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
fn digest_document(route: &Route, entry: &Document) -> Result<Vec<super::StretchDigest>, String> {
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

/// `bytes`, the workspace's governance as durable state kept it, checked as
/// the governance of `owner`'s workspace: what names another owner is not
/// this workspace's.
fn restored_governance(
    bytes: Option<&[u8]>,
    workspace: &str,
    owner: [u8; 32],
) -> Result<Option<CachedDocument>, String> {
    let governance = bytes.map(restored_workspace).transpose()?;
    if let Some(doc) = governance.as_deref()
        && super::receiving::owner_named_by(doc, workspace) != Some(owner)
    {
        return Err("durable admission namespace mismatch".into());
    }
    Ok(governance)
}
