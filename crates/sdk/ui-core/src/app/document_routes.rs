//! Native owner-route bridge. SDK retains signing, encryption and transport.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::collaboration::RouteSpec;
use automerge::transaction::Transactable;

fn matches_route(route: &RouteSpec, envelope: &collab_types::SignedOp) -> bool {
    collab_types::validate_document_payload_route(
        &route.app_id,
        &route.cluster_id.to_string(),
        route.record.as_deref().unwrap_or(""),
        envelope,
    )
    .is_ok()
}
impl App {
    pub(super) fn forward_session_notification(&mut self, event: &CollabEvent) {
        // Document traffic enters only the exact custody-checked merge door.
        // A mutable widget callback is not a read-only notification boundary.
        if !matches!(
            event,
            CollabEvent::Document { .. } | CollabEvent::ScrollbackBatch { .. }
        ) {
            crate::widget::handle_collab_event(self.root_widget.as_mut(), event);
        }
    }
    #[allow(
        clippy::vec_box,
        reason = "consume the existing SDK event envelope representation without copying it"
    )]
    pub(super) fn apply_document_scrollback(
        &mut self,
        ops: Vec<Box<collab_types::SignedOp>>,
        decrypted_payloads: &[Option<Vec<u8>>],
    ) {
        for (index, envelope) in ops.into_iter().enumerate() {
            self.apply_signed_document_envelope(
                &envelope,
                decrypted_payloads
                    .get(index)
                    .and_then(|payload| payload.as_deref()),
            );
        }
    }

    /// Local queue state, not an acknowledgement of remote delivery/durability.
    pub fn document_sync_status(&self) -> Option<&str> {
        self.document_routes
            .status
            .as_deref()
            .or(self.document_routes.primary.failure.as_deref())
            .or_else(|| {
                self.document_routes
                    .owners
                    .iter()
                    .find_map(|entry| entry.pending.failure.as_deref())
            })
            .or_else(|| {
                if self.document_routes.witness.is_none() {
                    Some("Collaboration is unavailable; remote changes are not synchronized.")
                } else if !self.document_routes.primary.is_empty()
                    || self
                        .document_routes
                        .owners
                        .iter()
                        .any(|entry| !entry.pending.is_empty())
                {
                    Some("Remote updates will apply when the current edit finishes.")
                } else {
                    None
                }
            })
    }
    pub(super) fn refresh_document_routes(&mut self) {
        let before = self.document_sync_status().map(str::to_owned);
        let witness = self.bound_broker_route().cloned();
        let joins = self.document_routes.reconcile(
            self.root_widget.as_ref(),
            witness,
            self.autosave_generation,
            &self.state.doc,
        );
        // The exact live owner map exists before any join can deliver callbacks.
        for route in joins {
            self.collab_manager.join_route(route);
        }
        if self.document_sync_status() != before.as_deref() {
            self.layout.layout_dirty = true;
        }
    }
    fn send_owned_document(&mut self, route: RouteSpec, request: CollabRequest) {
        #[cfg(test)]
        self.document_routes
            .sent
            .push((route.clone(), request.clone()));
        self.collab_manager.send_to_route(route, request);
    }
    pub(super) fn publish_owned_documents(&mut self) {
        self.refresh_document_routes();
        if self.document_routes.witness.is_none() {
            return;
        }
        let mut sends = vec![];
        for entry in &mut self.document_routes.owners {
            let Some(owner) = entry.owners.first() else {
                continue;
            };
            if entry.owners.iter().any(|other| !owner.same_document(other)) {
                entry.pending.failure = Some("Multiple different document owners claim this collaboration route. Synchronization is stopped.".into());
                continue;
            }
            if entry.pending.failure.is_some() {
                continue;
            }
            let Ok(mut document) = owner.document(&self.state.doc) else {
                continue;
            };
            if document.pending_ops() != 0 {
                continue;
            }
            let heads = crate::history_mode::sorted_heads(document.base_heads());
            if heads == entry.sent_heads && !entry.needs_snapshot {
                continue;
            }
            // Clone serialization cannot advance the canonical writer's or
            // another subscriber's incremental save cursor.
            let request = if entry.needs_snapshot {
                CollabRequest::document_snapshot(document.save())
            } else {
                CollabRequest::document_incremental(document.save_after(&entry.sent_heads))
            };
            entry.sent_heads = heads;
            entry.needs_snapshot = false;
            sends.push((entry.route.clone(), request));
        }
        for (route, request) in sends {
            self.send_owned_document(route, request);
        }
    }
    pub(super) fn drain_document_routes(&mut self) {
        self.refresh_document_routes();
        let mut changed = false;
        for entry in &mut self.document_routes.owners {
            let Some(owner) = entry.owners.first() else {
                continue;
            };
            if entry.owners.iter().any(|other| !owner.same_document(other)) {
                entry.pending.failure = Some("Multiple different document owners claim this collaboration route. Synchronization is stopped.".into());
                continue;
            }
            while let Some(mutation) = entry.pending.front() {
                match owner.merge_remote(mutation) {
                    Ok(Some(applied)) => {
                        entry.pending.pop_applied();
                        changed |= applied;
                        if applied {
                            let (store, checkout) = owner.store_checkout();
                            store.schedule_committed_history(&checkout.id);
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let reason = format!(
                            "Remote document could not be applied: {error}. Synchronization needs recovery."
                        );
                        if error.is_invalid_mutation() {
                            entry.pending.quarantine_invalid_front(reason);
                            continue;
                        }
                        entry.pending.failure = Some(reason);
                        break;
                    }
                }
            }
        }
        while let Some(mutation) = self.document_routes.primary.front() {
            match self.state.merge_remote_committed(mutation) {
                Ok(Some(applied)) => {
                    self.document_routes.primary.pop_applied();
                    changed |= applied;
                }
                Ok(None) => break,
                Err(error) => {
                    let reason = format!(
                        "Remote document could not be applied: {error}. Synchronization needs recovery."
                    );
                    if error.is_invalid_mutation() {
                        self.document_routes
                            .primary
                            .quarantine_invalid_front(reason);
                        continue;
                    }
                    self.document_routes.primary.failure = Some(reason);
                    break;
                }
            }
        }
        if changed {
            self.invalidate_widget_resource_cache();
            self.needs_sync = true;
            self.layout.layout_dirty = true;
        }
    }
    pub(super) fn apply_signed_document_envelope(
        &mut self,
        envelope: &collab_types::SignedOp,
        decrypted_payload: Option<&[u8]>,
    ) {
        // Before the first record checkout the launcher's unqualified route
        // carries room-index discovery, never the editable record document.
        if self.persistence.records.is_some() && self.room_collab_manager.is_none() {
            let _ = self.apply_room_index_document_envelope(envelope, decrypted_payload);
            return;
        }
        self.refresh_document_routes();
        let Some(witness) = self.document_routes.witness.as_ref() else {
            return;
        };
        // Signature/role/replay verification remains the SDK's responsibility.
        // This door additionally checks deterministic artifact, attestation and
        // encrypted channel against the actual installed route before decoding.
        let owned = self
            .document_routes
            .owners
            .iter()
            .position(|entry| matches_route(&entry.route, envelope));
        let primary_matches = matches_route(&witness.route, envelope)
            && !self.document_routes.retired_route(&witness.route)
            && match witness.route.record.as_deref() {
                Some(record) => self
                    .persistence
                    .records
                    .as_ref()
                    .and_then(|set| set.active())
                    .is_some_and(|id| id.as_str() == record),
                None => self
                    .persistence
                    .records
                    .as_ref()
                    .is_none_or(|set| set.active().is_none()),
            };
        if owned.is_none() && !primary_matches {
            return;
        }
        if envelope.encryption.is_some() && decrypted_payload.is_none() {
            return;
        }
        let mutation = match collab_types::DocumentMutation::decode(
            decrypted_payload.unwrap_or(&envelope.payload),
        ) {
            Ok(mutation) => mutation,
            Err(error) => {
                tracing::warn!(%error, "collab: invalid routed document mutation");
                return;
            }
        };
        let queue = if let Some(index) = owned {
            let Some(entry) = self.document_routes.owners.get_mut(index) else {
                return;
            };
            &mut entry.pending
        } else {
            &mut self.document_routes.primary
        };
        let accepted = queue.admit(mutation);
        if !accepted {
            self.layout.layout_dirty = true;
            tracing::warn!(status = ?queue.failure, "collab: routed document deferred queue refused update; resynchronization required");
        }
        self.drain_document_routes();
    }
}
