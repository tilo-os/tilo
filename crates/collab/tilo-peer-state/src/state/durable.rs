//! The durable record of a replica, and restoring one from it.

#[cfg(test)]
use super::DurableDocument;
#[cfg(test)]
use super::durable_before_owner::Durable;
use super::{
    AcceptedCheckpoint, CachedDocument, ChannelState, DURABLE_BASE_VERSION, DecodedCheckpoint,
    DeviceCert, Document, DurableDocumentRef, DurableV4, DurableV5, DurableV6, Ed25519PublicKey,
    ExactAdmissions, GOVERNANCE_LAYOUT_BEFORE, PeerCore, RevocationList, Route, Serialize, Shared,
    StatePeer, SystemTime, backend, check_governed_snapshot, durable_base_version,
    durable_before_owner, err, held, now_ns, process_default, workspace,
};

#[cfg(test)]
use automerge::{ReadDoc as _, transaction::Transactable as _};

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
    /// own, the same allocation while the base lasts, then the changes since
    /// it ([`CachedDocument::stored`]). Answers the head to go on with.
    fn put_workspace_document(
        &self,
        mut head: Vec<u8>,
        parts: &mut Vec<std::sync::Arc<[u8]>>,
    ) -> Result<Vec<u8>, String> {
        fn put<T: Serialize + ?Sized>(value: &T, out: Vec<u8>) -> Result<Vec<u8>, String> {
            postcard::to_extend(value, out).map_err(err)
        }
        match self.core.workspace_doc.as_ref().map(CachedDocument::stored) {
            Some((base, since)) => {
                head = put(&1u8, head)?;
                head = put(&base.len().saturating_add(since.len()), head)?;
                parts.push(head.into());
                parts.push(base);
                Ok(since.to_vec())
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
            documents.push(entry.encoded(route, || {
                let document = postcard::to_allocvec(&DurableDocumentRef {
                    route,
                    bytes: entry.doc.saved_bytes(),
                    watermarks: entry
                        .channel
                        .last_seq_per_author
                        .iter()
                        .map(|(key, seq)| (*key.as_bytes(), *seq))
                        .collect(),
                    encryption_required: entry.encryption_required,
                    covered: &entry.covered,
                    covered_snapshot: entry
                        .covered_snapshot
                        .as_ref()
                        .map(|snapshot| snapshot.0.bytes.as_slice()),
                })
                .map_err(err)?;
                let sealed = if entry.sealed.is_empty() {
                    None
                } else {
                    Some(entry.sealed.encode_into(route, Vec::new())?.into())
                };
                Ok(backend::EncodedDocument {
                    route: route.clone(),
                    document: document.into(),
                    sealed,
                })
            })?);
        }
        // The fields of `DurableV6`/`DurableV5`/`DurableV4`, in order.
        let mut parts: Vec<std::sync::Arc<[u8]>> = Vec::new();
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
        parts.push(head.into());
        parts.extend(documents.iter().map(|encoded| encoded.document.clone()));
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
            parts.push(tail.into());
            parts.push(settled);
            tail = rest;
        }
        if sealed > 0 {
            tail = put(&sealed, tail)?;
        }
        parts.push(tail.into());
        parts.extend(
            documents
                .iter()
                .filter_map(|encoded| encoded.sealed.clone()),
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
                bytes: entry.doc.saved(),
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
                    let (base, since) = doc.stored();
                    [&*base, &*since].concat()
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
        let base_before_owner = base == durable_before_owner::BASE_BEFORE_OWNER;
        if base != DURABLE_BASE_VERSION && base != GOVERNANCE_LAYOUT_BEFORE && !base_before_owner {
            return Err(format!("unsupported durable base version {base}"));
        }
        let (durable, pending, pending_bytes, exact, sealed) = if base_before_owner {
            match version {
                4 => durable_before_owner::v4(bytes)?,
                5 => durable_before_owner::v5(bytes)?,
                6 => durable_before_owner::v6(bytes)?,
                _ => return Err("unsupported durable admission version".into()),
            }
        } else {
            match version {
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
            }
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
            workspace_host: false,
            brought_up: false,
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
            let channel = ChannelState {
                last_seq_per_author: document
                    .watermarks
                    .into_iter()
                    .map(|(key, seq)| (Ed25519PublicKey::from_bytes(key), seq))
                    .collect(),
                ..ChannelState::default()
            };
            peer.core.documents.insert(
                document.route,
                Document {
                    doc: tilo_automerge_isolate::load(&document.bytes)
                        .map(CachedDocument::from)
                        .map_err(err)?,
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
