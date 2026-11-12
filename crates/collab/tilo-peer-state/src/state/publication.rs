//! Publishing exact checkpoints of a record with their coverage and receipts.

use super::{
    BTreeMap, DocumentAdmission, DocumentMutation, NeighbourMsg, Route, StatePeer, SystemTime, err,
};
use super::{PeerPublicationMut, backend};
use automerge::{ReadDoc as _, transaction::Transactable as _};
use collab_types::signer::DeviceSigner;

impl StatePeer {
    /// Publish exact checkpoints of the `dirty` routes and, in the same workspace
    /// change, their coverage plus the complete durable receipt set
    /// (`receipts` updated for each dirty route at `generation`). Equivalent
    /// to one `publish_document` per route followed by a receipt write, but
    /// one workspace change per durable commit and no interim receipt state.
    pub(super) fn publish_snapshots_with_receipts(
        &mut self,
        dirty: &[(Route, Vec<u8>)],
        mut receipts: BTreeMap<String, crate::durability::DurableReceipt>,
        generation: u64,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.validate_present_media_policy()?;
        let mut messages = Vec::with_capacity(dirty.len());
        let mut coverages = Vec::with_capacity(dirty.len());
        let mut sealed = Vec::new();
        for (route, bytes) in dirty {
            if self.sealed_route(route) {
                sealed.push(route.clone());
                continue;
            }
            let (message, coverage) = self.prepare_signed_mutation(
                &route.app,
                &route.record,
                DocumentMutation::Snapshot(bytes.clone()),
                key,
                now,
            )?;
            if self.core.admission_v5 {
                let NeighbourMsg::AppDocument { envelope, .. } = &message else {
                    return Err("prepared mutation is not an app document".into());
                };
                let admitted = DocumentAdmission::from_op(
                    &self.core.workspace,
                    &route.app,
                    &route.record,
                    envelope,
                )
                .map_err(err)?;
                // Reserve admission capacity before publishing coverage.
                self.core.exact_admissions.check(&admitted).map_err(err)?;
            }
            // A route another of the owner's devices covers keeps that
            // device's row: this checkpoint goes out as this device's own
            // change, with no coverage of its own to publish.
            coverages.extend(coverage);
            messages.push(message);
        }
        let covered = crate::coverage::entries(
            self.core
                .workspace_doc
                .as_deref()
                .ok_or("missing workspace governance")?,
        )?;
        let mut sealed_receipts = BTreeMap::new();
        for route in &sealed {
            if let Some(receipt) = self.sealed_receipt(route, &covered, generation)? {
                sealed_receipts.insert(
                    crate::coverage::route_key(&route.app, &route.record),
                    receipt,
                );
            }
        }
        crate::coverage::put_all_with_receipts(
            self.core
                .workspace_doc
                .as_deref_mut()
                .ok_or("missing workspace governance")?,
            coverages,
            |rows| {
                receipts.extend(sealed_receipts);
                for (route, _) in dirty {
                    let route_key = crate::coverage::route_key(&route.app, &route.record);
                    if sealed.contains(route) {
                        continue;
                    }
                    let covered = rows.get(&route_key).ok_or("missing published coverage")?;
                    receipts.insert(
                        route_key,
                        crate::durability::DurableReceipt::new(covered.clone(), generation)?,
                    );
                }
                Ok(receipts)
            },
        )?;
        for message in &messages {
            self.receive(key.device_public_key(), message.clone(), now)?;
        }
        Ok(messages)
    }
}

impl PeerPublicationMut<'_> {
    /// Sign and admit a snapshot of `record`, returning the message to publish.
    pub fn publish_document(
        self,
        app: &str,
        record: &str,
        bytes: Vec<u8>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.signed_mutation(app, record, DocumentMutation::Snapshot(bytes), key, now)
    }

    /// Sign and admit incremental changes to `record`, returning the message to publish.
    pub fn publish_incremental(
        self,
        app: &str,
        record: &str,
        bytes: Vec<u8>,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        self.signed_mutation(app, record, DocumentMutation::Incremental(bytes), key, now)
    }

    /// Sign and admit `mutation` of `record`, returning the message to publish.
    pub fn signed_mutation(
        self,
        app: &str,
        record: &str,
        mutation: DocumentMutation,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<NeighbourMsg, String> {
        let mut candidate = self.peer.clone();
        let message = candidate.signed_mutation_in_place(app, record, mutation, key, now)?;
        *self.peer = candidate;
        Ok(message)
    }

    /// Signed snapshots of every admitted document.
    pub fn signed_snapshots(
        self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.peer.clone();
        let messages = candidate.signed_snapshots_in_place(key, now)?;
        *self.peer = candidate;
        Ok(messages)
    }

    /// The governor publishes one consistent workspace coverage/checkpoint pair.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governor or a checkpoint cannot be produced.
    pub fn bootstrap_messages(
        self,
        key: &dyn DeviceSigner,
        now: SystemTime,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.peer.clone();
        let snapshots = candidate.signed_snapshots_in_place(key, now)?;
        let mut messages = vec![candidate.workspace_message()];
        messages.extend(snapshots);
        messages.extend(candidate.own_sealed_publications(key.device_public_key(), None));
        *self.peer = candidate;
        Ok(messages)
    }

    /// Prepare, but do not publish, a candidate bound to the next native commit.
    /// The caller must commit this entire state and outgoing batch before exposing it.
    ///
    /// # Errors
    ///
    /// Returns an error if `key` is not the governor or a checkpoint cannot be produced.
    pub fn durable_bootstrap_messages(
        self,
        key: &dyn DeviceSigner,
        now: SystemTime,
        generation: u64,
    ) -> Result<Vec<NeighbourMsg>, String> {
        let mut candidate = self.peer.clone();
        let snapshots = candidate.signed_snapshots_with_receipts(key, now, Some(generation))?;
        let workspace = candidate
            .core
            .workspace_doc
            .as_deref_mut()
            .ok_or("missing workspace governance")?;
        crate::durability::monotonic(self.peer.core.workspace_doc.as_deref(), workspace)?;
        let mut messages = vec![candidate.workspace_message()];
        messages.extend(snapshots);
        messages.extend(candidate.own_sealed_publications(key.device_public_key(), None));
        *self.peer = candidate;
        Ok(messages)
    }

    /// Prepare route-selective publication against the last committed peer.
    /// `SyncResp` here contains only workspace changes since `before` and MUST be retained
    /// as a delta. Application messages remain exact covered snapshots. The caller
    /// must durably commit the candidate and its baseline plus this tail before send.
    #[expect(
        clippy::too_many_lines,
        reason = "one candidate publication built against a single committed baseline"
    )]
    pub fn durable_mutation_messages(
        self,
        before: &StatePeer,
        key: &dyn DeviceSigner,
        now: SystemTime,
        generation: u64,
    ) -> Result<Vec<NeighbourMsg>, String> {
        self.peer.admissions_since(before).map_err(err)?;
        if self.peer.core.workspace != before.core.workspace
            || self.peer.core.owner != before.core.owner
        {
            return Err("publication cannot replace the session namespace".into());
        }
        self.peer
            .permissions(key.device_public_key(), now)
            .map_err(|error| error.to_string())?;
        if self.peer.incomplete_reason().is_some() {
            return Err("cannot publish incomplete encryption state".into());
        }
        let before_workspace = before
            .core
            .workspace_doc
            .as_ref()
            .ok_or("missing prior governance")?;
        let mut candidate = self.peer.clone();
        for (route, prior) in &before.core.documents {
            let document = candidate
                .core
                .documents
                .get(route)
                .ok_or("publication discards a record")?;
            // A record still shared with `before` discards nothing.
            if backend::StoredDocument::ptr_eq(prior, document) {
                continue;
            }
            if prior
                .doc
                .base_heads()
                .iter()
                .any(|head| document.doc.get_change_by_hash(head).is_none())
                || prior
                    .channel
                    .last_seq_per_author
                    .iter()
                    .any(|(author, seq)| {
                        document
                            .channel
                            .last_seq_per_author
                            .get(author)
                            .copied()
                            .unwrap_or(0)
                            < *seq
                    })
            {
                return Err("publication discards document history or replay floors".into());
            }
        }
        let workspace = candidate
            .core
            .workspace_doc
            .as_mut()
            .ok_or("missing workspace governance")?;
        let before_heads = before_workspace.base_heads();
        if before_heads
            .iter()
            .any(|head| workspace.get_change_by_hash(head).is_none())
        {
            return Err("publication discards workspace history".into());
        }
        crate::coverage::monotonic(Some(before_workspace), workspace)?;
        crate::durability::monotonic(Some(before_workspace), workspace)?;
        let old_coverage = crate::coverage::entries(before_workspace)?;
        let coverage = crate::coverage::entries(workspace)?;
        let old_receipts = crate::durability::entries(before_workspace)?;
        let receipts = crate::durability::entries(workspace)?;
        let mut dirty = Vec::new();
        for (route, document) in &candidate.core.documents {
            let route_key = crate::coverage::route_key(&route.app, &route.record);
            let changed = before.core.documents.get(route).is_none_or(|prior| {
                // A record this change did not touch is still shared.
                let same = backend::StoredDocument::ptr_eq(prior, document);
                (!same
                    && (prior.doc.base_heads() != document.doc.base_heads()
                        || prior.channel.last_seq_per_author
                            != document.channel.last_seq_per_author
                        || prior.encryption_required != document.encryption_required
                        || prior.covered != document.covered
                        || prior.covered_snapshot != document.covered_snapshot))
                    || old_coverage.get(&route_key) != coverage.get(&route_key)
                    || old_receipts.get(&route_key) != receipts.get(&route_key)
            });
            if changed {
                dirty.push((route.clone(), document.doc.saved()));
            }
        }
        let snapshots = if dirty.is_empty() {
            Vec::new()
        } else {
            candidate.publish_snapshots_with_receipts(&dirty, receipts, generation, key, now)?
        };
        let workspace = candidate
            .core
            .workspace_doc
            .as_mut()
            .ok_or("missing workspace governance")?;
        crate::coverage::monotonic(Some(before_workspace), workspace)?;
        crate::durability::monotonic(Some(before_workspace), workspace)?;
        let changes = workspace
            .get_changes(&before_heads)
            .into_iter()
            .map(crate::workspace::sent_change)
            .collect::<Vec<_>>();
        let mut messages = Vec::new();
        if !changes.is_empty() {
            messages.push(NeighbourMsg::SyncResp {
                changes,
                room: self.peer.core.workspace.clone(),
            });
        }
        messages.extend(snapshots);
        messages.extend(candidate.own_sealed_publications(key.device_public_key(), Some(before)));
        *self.peer = candidate;
        Ok(messages)
    }
}
