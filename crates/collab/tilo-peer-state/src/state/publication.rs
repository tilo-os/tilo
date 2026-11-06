//! Publishing exact checkpoints of a record with their coverage and receipts.

use super::{
    BTreeMap, DocumentAdmission, DocumentMutation, NeighbourMsg, Route, SigningKey, StatePeer,
    SystemTime, err,
};

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
        key: &SigningKey,
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
            self.receive(key.verifying_key().to_bytes(), message.clone(), now)?;
        }
        Ok(messages)
    }
}
