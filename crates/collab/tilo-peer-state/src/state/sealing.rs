//! Signing, sealing and publishing: the changes and snapshots this peer sends.

use super::{
    AutoCommit, BTreeMap, DocumentAdmission, DocumentMutation, Ed25519PublicKey, NeighbourMsg,
    Route, SeedState, StatePeer, SystemTime, empty_genesis, err, peer_handle,
};
use collab_types::signer::DeviceSigner;

use automerge::transaction::Transactable as _;

impl StatePeer {
    pub fn workspace_message(&self) -> NeighbourMsg {
        let changes = self
            .core
            .workspace_doc
            .clone()
            .map(|mut doc| {
                doc.get_changes(&[])
                    .into_iter()
                    .map(crate::workspace::sent_change)
                    .collect()
            })
            .unwrap_or_default();
        NeighbourMsg::SyncResp {
            changes,
            room: self.core.workspace.clone(),
        }
    }
    /// Install the governed seed workspace and documents as the authority.
    pub fn install_seed(
        &mut self,
        seed: SeedState,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<(), String> {
        self.receive(
            key.device_public_key(),
            NeighbourMsg::SyncResp {
                changes: seed.workspace_changes,
                room: self.core.workspace.clone(),
            },
            now,
        )?;
        for document in seed.documents {
            self.publication_mut().publish_document(
                &document.app_id,
                &document.record_id,
                document.bytes,
                key,
                now,
            )?;
        }
        Ok(())
    }
    pub(super) fn signed_mutation_in_place(
        &mut self,
        app: &str,
        record: &str,
        mutation: DocumentMutation,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.validate_present_media_policy()?;
        let (message, coverage) = self.prepare_signed_mutation(app, record, mutation, key, now)?;
        if self.core.admission_v5 {
            let NeighbourMsg::AppDocument { envelope, .. } = &message else {
                return Err("prepared mutation is not an app document".into());
            };
            let admitted = DocumentAdmission::from_op(&self.core.workspace, app, record, envelope)
                .map_err(err)?;
            // Reserve admission capacity before publishing coverage into live governance.
            self.core.exact_admissions.check(&admitted).map_err(err)?;
        }
        if let Some(coverage) = coverage {
            crate::coverage::put(
                self.core
                    .workspace_doc
                    .as_deref_mut()
                    .ok_or("missing workspace governance")?,
                coverage,
            )?;
        }
        self.receive(key.device_public_key(), message.clone(), now)?;
        Ok(message)
    }
    #[expect(
        clippy::needless_pass_by_value,
        reason = "callers hand over freshly built mutations, as `signed_mutation` takes ownership"
    )]
    pub(super) fn prepare_signed_mutation(
        &self,
        app: &str,
        record: &str,
        mutation: DocumentMutation,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<(NeighbourMsg, Option<crate::coverage::Coverage>), String> {
        let author = Ed25519PublicKey::from_bytes(key.device_public_key());
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let mut op = self.draft_write(
            (app, record),
            &mutation.encode().map_err(err)?,
            None,
            *author.as_bytes(),
            0,
            now,
        )?;
        let seq = op.seq;
        collab_types::sign_op(&mut op, key).map_err(err)?;
        let message = NeighbourMsg::AppDocument {
            peer: peer_handle(author.as_bytes()),
            app_id: app.into(),
            cluster_id: self.core.workspace.clone(),
            record_id: record.into(),
            envelope: Box::new(op),
        };
        let coverage = if let DocumentMutation::Snapshot(bytes) = &mutation {
            // A record's genesis is not a checkpoint: it is started empty, so
            // there is no retained history for the workspace to acknowledge.
            // Two devices that start the same record each publish their own
            // genesis, and they merge into the one empty record; two coverage
            // rows for one route would not, since a route's checkpoint row
            // keeps the writer it was first acknowledged with.
            if !self.core.documents.contains_key(&route) && empty_genesis(bytes) {
                return Ok((message, None));
            }
            // The workspace governs the checkpoints its owner's devices
            // publish, and only theirs: a record any other writer publishes
            // is that writer's own change, merged into what a device holds.
            if !self.signed_by_owner(author.as_bytes()) {
                return Ok((message, None));
            }
            // And of a route, the workspace governs the one its coverage row
            // names: the first of the owner's devices to cover it. Another of
            // their devices publishing the record as it holds it is that
            // device's own change too, so the row stays one writer's.
            if self
                .governed_coverage(&route)?
                .is_some_and(|row| row.author != *author.as_bytes())
            {
                return Ok((message, None));
            }
            let snapshot = AutoCommit::load(bytes).map_err(err)?;
            let mut watermarks = self
                .core
                .documents
                .get(&route)
                .map(|entry| {
                    entry
                        .channel
                        .last_seq_per_author
                        .iter()
                        .map(|(key, value)| (hex::encode(key.as_bytes()), *value))
                        .collect::<BTreeMap<_, _>>()
                })
                .unwrap_or_default();
            watermarks.insert(hex::encode(author.as_bytes()), seq);
            let NeighbourMsg::AppDocument { envelope, .. } = &message else {
                return Err("prepared mutation is not an app document".into());
            };
            Some(crate::coverage::Coverage {
                workspace: self.core.workspace.clone(),
                app: app.into(),
                record: record.into(),
                payload_hash: *blake3::hash(&envelope.payload).as_bytes(),
                heads: snapshot
                    .base_heads()
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                author: *author.as_bytes(),
                sequence: seq,
                watermarks,
            })
        } else {
            None
        };
        Ok((message, coverage))
    }
    /// An encrypted record this replica keeps as sealed history: no governed
    /// snapshot can carry it, since a cleartext checkpoint would downgrade it.
    pub(super) fn sealed_route(&self, route: &Route) -> bool {
        self.core
            .documents
            .get(route)
            .is_some_and(|document| document.encryption_required)
    }
    /// The governor's receipt for `route`'s sealed history: its last
    /// checkpoint coverage in `rows`, acknowledging every sealed write this
    /// replica durably holds through the record's sequence watermarks.
    /// `None` for a record without checkpoint coverage.
    pub(super) fn sealed_receipt(
        &self,
        route: &Route,
        rows: &BTreeMap<String, crate::coverage::Coverage>,
        generation: u64,
    ) -> Result<Option<crate::durability::DurableReceipt>, String> {
        let Some(mut coverage) = rows
            .get(&crate::coverage::route_key(&route.app, &route.record))
            .cloned()
        else {
            return Ok(None);
        };
        let document = self
            .core
            .documents
            .get(route)
            .ok_or("sealed receipt for a record this state does not hold")?;
        for (author, sequence) in &document.channel.last_seq_per_author {
            let watermark = coverage
                .watermarks
                .entry(hex::encode(author.as_bytes()))
                .or_default();
            *watermark = (*watermark).max(*sequence);
        }
        crate::durability::DurableReceipt::new(coverage, generation).map(Some)
    }
    /// Whether this replica holds a record in the clear: one a catch-up hands
    /// on as a snapshot this device signs, taking a number of its sequence.
    pub(crate) fn holds_clear_records(&self) -> bool {
        self.core
            .documents
            .keys()
            .any(|route| !self.sealed_route(route))
    }
    pub(super) fn signed_snapshots_in_place(
        &mut self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.signed_snapshots_with_receipts(key, now, None)
    }
    /// Publish exact checkpoints of every document. With `receipts_at`, the
    /// same workspace change also records a durable receipt at that generation for
    /// every covered route (one change per durable bootstrap).
    pub(super) fn signed_snapshots_with_receipts(
        &mut self,
        key: &dyn DeviceSigner,
        now: SystemTime,
        receipts_at: Option<u64>,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.validate_present_media_policy()?;
        let prepared = self
            .core
            .documents
            .iter()
            .filter(|(route, _)| !self.sealed_route(route))
            .map(|(route, entry)| {
                self.prepare_signed_mutation(
                    &route.app,
                    &route.record,
                    DocumentMutation::Snapshot(entry.doc.saved()),
                    key,
                    now,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (messages, coverages): (Vec<_>, Vec<_>) = prepared.into_iter().unzip();
        let coverages = coverages.into_iter().flatten().collect::<Vec<_>>();
        let sealed = self
            .core
            .documents
            .keys()
            .filter(|route| self.sealed_route(route))
            .map(|route| (crate::coverage::route_key(&route.app, &route.record), route))
            .collect::<BTreeMap<_, _>>();
        let mut sealed_receipts = BTreeMap::new();
        if let Some(generation) = receipts_at {
            let rows = crate::coverage::entries(
                self.core
                    .workspace_doc
                    .as_deref()
                    .ok_or("missing workspace governance")?,
            )?;
            for (route_key, route) in &sealed {
                if let Some(receipt) = self.sealed_receipt(route, &rows, generation)? {
                    sealed_receipts.insert(route_key.clone(), receipt);
                }
            }
        }
        let workspace = self
            .core
            .workspace_doc
            .as_deref_mut()
            .ok_or("missing workspace governance")?;
        match receipts_at {
            Some(generation) => {
                crate::coverage::put_all_with_receipts(workspace, coverages, |rows| {
                    let mut receipts = sealed_receipts;
                    for (route, coverage) in rows {
                        if !sealed.contains_key(route) {
                            receipts.insert(
                                route.clone(),
                                crate::durability::DurableReceipt::new(
                                    coverage.clone(),
                                    generation,
                                )?,
                            );
                        }
                    }
                    Ok(receipts)
                })?;
            }
            None if !coverages.is_empty() => crate::coverage::put_all(workspace, coverages)?,
            None => {}
        }
        for message in &messages {
            self.receive(key.device_public_key(), message.clone(), now)?;
        }
        Ok(messages)
    }
}
