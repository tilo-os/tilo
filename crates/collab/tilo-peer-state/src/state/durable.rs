//! The durable record of a replica, and restoring one from it.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use super::{
    AcceptedCheckpoint, CachedDocument, DecodedCheckpoint, DeviceCert, Document, Ed25519PublicKey,
    ExactAdmissions, InboundState, PeerCore, RevocationList, Route, Shared, StatePeer, SystemTime,
    backend, check_governed_snapshot, err, held, now_ns, process_default, workspace,
};
use super::{DurableDocument, SealedHistory};
use crate::NeighbourMsg;

#[cfg(test)]
use automerge::{ReadDoc as _, transaction::Transactable as _};

/// The base whose governance holds device rows as JSON and each receipt
/// with its whole invitation: the same fields as [`DURABLE_BASE_VERSION`],
/// its governance brought up as it is read ([`crate::workspace::layout`]).
pub const GOVERNANCE_LAYOUT_BEFORE: u32 = 4;

/// The durable base layout. Version 5 keeps the governance's device rows
/// and receipts compact ([`crate::workspace::layout`]); version 4 is the same
/// base around the earlier governance layout.
pub const DURABLE_BASE_VERSION: u32 = 5;

#[derive(Serialize, Deserialize)]
pub struct Durable {
    pub(super) version: u32,
    pub(super) workspace: String,
    pub(super) owner: [u8; 32],
    pub(super) workspace_bytes: Option<Vec<u8>>,
    pub(super) documents: Vec<DurableDocument>,
    pub(super) receipts: BTreeMap<String, Vec<u8>>,
    pub(super) certificates: Vec<DeviceCert>,
    pub(super) revocations: Vec<tilo_user_identity::RevocationEntry>,
}
#[derive(Serialize, Deserialize)]
pub struct DurableV4 {
    pub(super) version: u32,
    pub(super) base: Durable,
    pub(super) pending: VecDeque<([u8; 32], NeighbourMsg, bool)>,
    pub(super) pending_bytes: usize,
}

// The v4 shape stays frozen. Exact evidence is an explicitly selected addition.
#[derive(Serialize, Deserialize)]
pub struct DurableV5<'a> {
    pub(super) version: u32,
    pub(super) base: DurableV4,
    #[serde(borrow)]
    pub(super) exact: &'a [u8],
}

/// V5 and the sealed history of each encrypted record. Written only while a
/// record holds sealed writes, so every other state keeps its v5 bytes.
#[derive(Serialize, Deserialize)]
pub struct DurableV6<'a> {
    pub(super) version: u32,
    pub(super) base: DurableV4,
    #[serde(borrow)]
    pub(super) exact: &'a [u8],
    pub(super) sealed: SealedHistory,
}

/// The version of a durable state's base, read from the versions that lead
/// the bytes, never by decoding a layout and retrying another.
///
/// How many versions lead the base's depends on the envelope: version 4
/// names its base's version second, and versions 5 and 6 wrap a version 4
/// envelope, which names it after its own.
pub fn durable_base_version(bytes: &[u8]) -> Result<u32, String> {
    let (envelope, rest) = version(bytes)?;
    let rest = match envelope {
        4 => rest,
        5 | 6 => version(rest)?.1,
        _ => return Err("unsupported durable admission version".into()),
    };
    Ok(version(rest)?.0)
}

/// The version of a durable state's base, read from the versions that lead
/// it.
fn version(bytes: &[u8]) -> Result<(u32, &[u8]), String> {
    postcard::take_from_bytes::<u32>(bytes).map_err(|error| error.to_string())
}

/// The digests of a replica's durable parts, each kept with its part: the
/// part's allocation is alive while it is kept, so its address names it.
#[derive(Clone, Default)]
pub(crate) struct PartDigests(std::sync::Arc<parking_lot::Mutex<Digested>>);
/// Each part's digest, by the part's address and length.
type Digested = std::collections::HashMap<(usize, usize), (std::sync::Arc<[u8]>, [u8; 32])>;

/// A stretch of the durable encoding ([`StatePeer::durable_parts`]).
#[derive(Clone)]
pub(super) enum Piece {
    /// A part with bytes of its own.
    Part(std::sync::Arc<[u8]>),
    /// A document's changes since its base, in runs, and the digest of the
    /// runs' bytes one after the other: digested as one stretch, as a
    /// restored document holds them in one run.
    Since(Vec<std::sync::Arc<[u8]>>, [u8; 32]),
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
    /// written by any participant or store whose device this replica knows.
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
    /// its stretches ([`Piece`]), in order. A part an earlier call digested
    /// and this state still holds is not read again, and a document's
    /// changes since its base carry their digest, so a write pays for
    /// digesting what it changed, not the record's whole history.
    pub fn durable_hash(&self) -> Result<[u8; 32], String> {
        let pieces = self.durable_pieces()?;
        let mut kept = std::mem::take(&mut *self.durable_digests.0.lock());
        let mut next = std::collections::HashMap::with_capacity(pieces.len());
        let mut hasher = blake3::Hasher::new();
        for piece in pieces {
            let (len, digest) = match piece {
                Piece::Part(part) => {
                    let at = (part.as_ptr() as usize, part.len());
                    let digest = kept.remove(&at).map_or_else(
                        || {
                            crate::write_work::read(part.len());
                            *blake3::hash(&part).as_bytes()
                        },
                        |(_, digest)| digest,
                    );
                    let len = part.len();
                    next.insert(at, (part, digest));
                    (len, digest)
                }
                Piece::Since(runs, digest) => (runs.iter().map(|run| run.len()).sum(), digest),
            };
            hasher.update(&(len as u64).to_le_bytes());
            hasher.update(&digest);
        }
        *self.durable_digests.0.lock() = next;
        Ok(*hasher.finalize().as_bytes())
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
    ) -> Result<Vec<u8>, String> {
        fn put<T: Serialize + ?Sized>(value: &T, out: Vec<u8>) -> Result<Vec<u8>, String> {
            postcard::to_extend(value, out).map_err(err)
        }
        match self.core.workspace_doc.as_ref().map(CachedDocument::stored) {
            Some((base, since, digest)) => {
                let since_len = since.iter().map(|run| run.len()).sum::<usize>();
                head = put(&1u8, head)?;
                head = put(&base.len().saturating_add(since_len), head)?;
                parts.push(Piece::Part(head.into()));
                parts.push(Piece::Part(base));
                parts.push(Piece::Since(since, digest));
                Ok(Vec::new())
            }
            None => put(&0u8, head),
        }
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
            .durable_pieces()?
            .into_iter()
            .flat_map(|piece| match piece {
                Piece::Part(part) => vec![part],
                Piece::Since(runs, _) => runs,
            })
            .collect())
    }

    /// [`Self::durable_parts`] as the stretches [`Self::durable_hash`] digests.
    fn durable_pieces(&self) -> Result<Vec<Piece>, String> {
        fn put<T: Serialize + ?Sized>(value: &T, out: Vec<u8>) -> Result<Vec<u8>, String> {
            postcard::to_extend(value, out).map_err(err)
        }
        let sealed = self
            .core
            .documents
            .values()
            .filter(|entry| !entry.sealed.is_empty())
            .count();
        if sealed > 0 && !self.core.admission_v5 {
            return Err("sealed record history requires durable admission v5".into());
        }
        let mut documents = Vec::with_capacity(self.core.documents.len());
        for (route, entry) in &self.core.documents {
            documents.push(entry.encoded(route, || encode_document(route, entry))?);
        }
        // The fields of `DurableV6`/`DurableV5`/`DurableV4`, in order.
        let mut parts: Vec<Piece> = Vec::new();
        let mut head = Vec::new();
        if sealed > 0 {
            head = put(&6u32, head)?;
        } else if self.core.admission_v5 {
            head = put(&5u32, head)?;
        }
        head = put(&4u32, head)?;
        head = put(
            &(DURABLE_BASE_VERSION, &self.core.workspace, &self.core.owner),
            head,
        )?;
        head = self.put_workspace_document(head, &mut parts)?;
        head = put(&documents.len(), head)?;
        parts.push(Piece::Part(head.into()));
        parts.extend(
            documents
                .iter()
                .flat_map(|encoded| encoded.document.iter().cloned()),
        );
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
        if self.core.admission_v5 {
            let (settled, rest) = self.core.exact_admissions.encode_parts().map_err(err)?;
            tail = put(&settled.len().saturating_add(rest.len()), tail)?;
            parts.push(Piece::Part(tail.into()));
            parts.push(Piece::Part(settled));
            tail = rest;
        }
        if sealed > 0 {
            tail = put(&sealed, tail)?;
        }
        parts.push(Piece::Part(tail.into()));
        parts.extend(
            documents
                .iter()
                .filter_map(|encoded| encoded.sealed.as_ref())
                .flatten()
                .cloned()
                .map(Piece::Part),
        );
        Ok(parts)
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
        let value = DurableV4 {
            version: 4,
            base: Durable {
                version: DURABLE_BASE_VERSION,
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
            },
            pending: self.core.pending.clone(),
            pending_bytes: self.core.pending_bytes,
        };
        let sealed: Vec<_> = self
            .core
            .documents
            .iter()
            .filter(|(_, entry)| !entry.sealed.is_empty())
            .map(|(route, entry)| (route.clone(), entry.sealed.to_vec()))
            .collect();
        if self.core.admission_v5 && !sealed.is_empty() {
            let exact = self.core.exact_admissions.encode().map_err(err)?;
            postcard::to_allocvec(&DurableV6 {
                version: 6,
                base: value,
                exact: &exact,
                sealed,
            })
            .map_err(err)
        } else if !sealed.is_empty() {
            Err("sealed record history requires durable admission v5".into())
        } else if self.core.admission_v5 {
            let exact = self.core.exact_admissions.encode().map_err(err)?;
            postcard::to_allocvec(&DurableV5 {
                version: 5,
                base: value,
                exact: &exact,
            })
            .map_err(err)
        } else {
            postcard::to_allocvec(&value).map_err(err)
        }
    }

    /// Restore a peer from durable state produced by [`StatePeer::durable`].
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public API: callers hand over the pinned revocation list"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "one durable restore across every stored schema version"
    )]
    pub fn restore(
        bytes: &[u8],
        workspace: &str,
        owner_device: DeviceCert,
        roster: Vec<DeviceCert>,
        revocations: RevocationList,
        now: SystemTime,
    ) -> Result<Self, String> {
        let (version, _) = postcard::take_from_bytes::<u32>(bytes).map_err(err)?;
        let exact_admissions;
        // Which base layout: the three integers both begin with, read before
        // anything that differs. Never decode one and retry another.
        let base = durable_base_version(bytes)?;
        if base != DURABLE_BASE_VERSION && base != GOVERNANCE_LAYOUT_BEFORE {
            return Err(format!("unsupported durable base version {base}"));
        }
        let (durable, pending, pending_bytes, exact, sealed) = match version {
            4 => {
                let value: DurableV4 = postcard::from_bytes(bytes).map_err(err)?;
                (
                    value.base,
                    value.pending,
                    value.pending_bytes,
                    ExactAdmissions::default(),
                    Vec::new(),
                )
            }
            5 => {
                let (value, tail): (DurableV5<'_>, _) =
                    postcard::take_from_bytes(bytes).map_err(err)?;
                if !tail.is_empty() || value.version != 5 || value.base.version != 4 {
                    return Err("invalid native v5 durable envelope".into());
                }
                (
                    value.base.base,
                    value.base.pending,
                    value.base.pending_bytes,
                    ExactAdmissions::decode(value.exact).map_err(err)?,
                    Vec::new(),
                )
            }
            6 => {
                let (value, tail): (DurableV6<'_>, _) =
                    postcard::take_from_bytes(bytes).map_err(err)?;
                if !tail.is_empty()
                    || value.version != 6
                    || value.base.version != 4
                    || value.sealed.is_empty()
                {
                    return Err("invalid native v6 durable envelope".into());
                }
                (
                    value.base.base,
                    value.base.pending,
                    value.base.pending_bytes,
                    ExactAdmissions::decode(value.exact).map_err(err)?,
                    value.sealed,
                )
            }
            _ => return Err("unsupported durable admission version".into()),
        };
        exact_admissions = exact;
        if (durable.version != DURABLE_BASE_VERSION && durable.version != GOVERNANCE_LAYOUT_BEFORE)
            || durable.workspace != workspace
            || durable.owner != owner_device.user_public_key
        {
            return Err("durable admission namespace mismatch".into());
        }
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
                pending,
                pending_bytes,
                exact_admissions,
                admission_v5: version >= 5,
                native_cut_publisher: None,
            },
            surfaces: process_default().0,
            app_surfaces: process_default().1,
            missed: held::Missed::default(),
            gathering: super::compaction_set::Gathering::default(),
            workspace_host: false,
            brought_up: false,
            durable_digests: PartDigests::default(),
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
            .map(|bytes| {
                // Isolated load leaves the document in a worker so the first
                // inbound change does not pay a full rebuild.
                tilo_automerge_isolate::load(&bytes)
                    .map(CachedDocument::from)
                    .map_err(err)
                    .and_then(|doc| doc.stored_as(&bytes))
            })
            .transpose()?;
        // A governance a build before this layout wrote is brought up once,
        // here, and is never read in the layout it was written in.
        if base != DURABLE_BASE_VERSION
            && let Some(held) = peer.core.workspace_doc.as_deref()
        {
            let actor = peer.workspace_actor_for(&peer.core.owner);
            let brought = workspace::layout::bring_up(held, actor)?;
            peer.core.workspace_doc = Some(CachedDocument::from(brought));
            peer.brought_up = true;
        }
        for document in durable.documents {
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
        for (route, ops) in sealed {
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
        // Every exact entry and every compaction mark lies within its
        // route's watermark.
        let index = &peer.core.exact_admissions;
        let keys: Vec<_> = index
            .iter()
            .map(|(key, _)| key.clone())
            .chain(index.marks().map(|(writer, mark)| writer.at(mark)))
            .collect();
        for key in &keys {
            let floor = peer
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
            if key.workspace != peer.core.workspace
                || floor.is_none_or(|floor| key.sequence > floor)
            {
                return Err("exact admission evidence exceeds restored route/floor".into());
            }
        }
        Ok(peer)
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
    let (base, since, digest) = entry.doc.stored();
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
            Piece::Since(since, digest),
            Piece::Part(rest.into()),
        ],
        sealed,
    })
}
