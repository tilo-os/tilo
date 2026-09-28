//! Native owner-route bridge. SDK retains signing, encryption and transport.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::collaboration::RouteSpec;

/// The user-visible status a transport outcome implies for a route: `Some`
/// replaces the current one (clearing it on success), `None` leaves it (a
/// transient refusal is retried silently).
#[allow(
    clippy::option_option,
    reason = "three outcomes: replace with a message, clear it, or leave the current status"
)]
pub(super) fn outbound_failure(
    result: &Result<(), crate::collaboration::CollabSendError>,
) -> Option<Option<String>> {
    match result {
        Ok(()) => Some(None),
        Err(crate::collaboration::CollabSendError::TooLarge { actual, max }) => {
            Some(Some(format!(
                "Changes are no longer shared: one change is {}, above the {} collaboration limit. \
             They remain on this device.",
                crate::format::bytes(u64::try_from(*actual).unwrap_or(u64::MAX)),
                crate::format::bytes(u64::try_from(*max).unwrap_or(u64::MAX)),
            )))
        }
        Err(_) => None,
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static DOCUMENT_ROUTE_REFRESHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn validated_route_payload(
    route: &RouteSpec,
    envelope: &collab_types::SignedOp,
) -> Option<collab_types::WholeDocumentPayload> {
    collab_types::validate_whole_document_payload_route(
        &route.app_id,
        &route.cluster_id.to_string(),
        route.record.as_deref().unwrap_or(""),
        envelope,
    )
    .ok()
}

/// [`validated_route_payload`] for an envelope as it reaches the endpoint:
/// a reassembled cleartext transfer arrives on its last segment's envelope
/// with the complete mutation beside it. The segment envelope must belong to
/// `route`, and the complete payload must be a current-format mutation (the
/// only format that is ever segmented).
pub(super) fn validated_route_payload_with(
    route: &RouteSpec,
    envelope: &collab_types::SignedOp,
    decrypted_payload: Option<&[u8]>,
) -> Option<collab_types::WholeDocumentPayload> {
    let (None, Some(complete)) = (envelope.encryption.as_ref(), decrypted_payload) else {
        return validated_route_payload(route, envelope);
    };
    let segment = collab_types::validate_document_payload_route(
        &route.app_id,
        &route.cluster_id.to_string(),
        route.record.as_deref().unwrap_or(""),
        envelope,
    )
    .ok()?;
    if !matches!(
        segment,
        collab_types::RoutedDocumentPayload::CleartextSegment(_)
    ) {
        return None;
    }
    let mutation = collab_types::DocumentMutation::decode(complete).ok()?;
    Some(collab_types::WholeDocumentPayload::Cleartext(mutation))
}

pub(super) fn decode_routed_mutation(
    route: &RouteSpec,
    payload: collab_types::WholeDocumentPayload,
    decrypted_payload: Option<&[u8]>,
) -> Result<collab_types::DocumentMutation, collab_types::DocumentMutationError> {
    match payload {
        collab_types::WholeDocumentPayload::Cleartext(mutation) => Ok(mutation),
        collab_types::WholeDocumentPayload::Encrypted { channel, .. } => {
            let decrypted_payload =
                decrypted_payload.ok_or(collab_types::DocumentMutationError::RouteMismatch)?;
            let mutation = collab_types::DocumentMutation::decode(decrypted_payload)?;
            let record = route.record.as_deref().unwrap_or("");
            if collab_types::document_channel_id(
                &route.app_id,
                &route.cluster_id.to_string(),
                record,
            ) != channel
            {
                return Err(collab_types::DocumentMutationError::RouteMismatch);
            }
            Ok(mutation)
        }
    }
}
impl App {
    pub(super) fn forward_session_notification(&mut self, event: &CollabEvent) {
        // Document traffic enters only the exact custody-checked merge door.
        // A mutable widget callback is not a read-only notification boundary.
        if !matches!(
            event,
            CollabEvent::Document { .. }
                | CollabEvent::ScrollbackBatch { .. }
                | CollabEvent::CommittedDocument { .. }
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
        self.action_publication_failure.as_deref().or(self
            .document_routes
            .status
            .as_deref()
            .or(self.document_routes.primary.failure.as_deref())
            .or(self.document_routes.outbound.as_deref())
            .or_else(|| {
                self.document_routes
                    .owners
                    .iter()
                    .find_map(|entry| entry.pending.failure.as_deref())
            })
            .or_else(|| {
                self.document_routes
                    .owners
                    .iter()
                    .find_map(|entry| entry.outbound_failure.as_deref())
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
            }))
    }
    /// Queue acceptance may have advanced producer heads before a later
    /// worker failure. Restore history obligations before observing another
    /// route or attempting publication; the real broker still authorizes retry.
    pub(super) fn restore_failed_document_publications(&mut self) {
        let primary_failed = self.collaboration.manager.document_publication_failed();
        let room_failed = self
            .collaboration
            .room_manager
            .as_ref()
            .is_some_and(CollaborationManager::document_publication_failed);
        if primary_failed {
            self.restore_primary_publication_obligations();
        }
        if (primary_failed || room_failed)
            && let Some(room) = self.persistence.room_index.as_mut()
        {
            room.advance_outbound(Vec::new());
        }
    }

    fn restore_primary_publication_obligations(&mut self) {
        self.state.advance_outbound(Vec::new());
        self.actions.bound_record_sync = None;
        for owner in &mut self.document_routes.owners {
            owner.sent_heads.clear();
            owner.needs_snapshot = true;
        }
    }

    pub(super) fn refresh_document_routes(&mut self) {
        self.restore_failed_document_publications();
        #[cfg(test)]
        DOCUMENT_ROUTE_REFRESHES.set(DOCUMENT_ROUTE_REFRESHES.get() + 1);
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
            self.collaboration.manager.join_route(route);
        }
        if self.document_sync_status() != before.as_deref() {
            self.layout.layout_dirty = true;
        }
    }
    fn send_owned_document(
        &mut self,
        route: RouteSpec,
        request: CollabRequest,
    ) -> Result<(), crate::collaboration::CollabSendError> {
        #[cfg(test)]
        if let Some(refusal) = self.document_routes.refuse_for_test.clone() {
            return Err(refusal);
        }
        #[cfg(test)]
        let observation = (route.clone(), request.clone());
        self.collaboration
            .manager
            .try_send_to_route(route, request)?;
        #[cfg(test)]
        self.document_routes.sent.push(observation);
        Ok(())
    }

    /// Ordinary background/history publication remains best effort. Only a
    /// directed terminal requires the explicit complete-queue result below.
    #[cfg(any(test, feature = "native"))]
    pub(super) fn publish_owned_documents(&mut self) {
        let _ = self.queue_owned_documents();
    }

    /// Enqueue all changed live owned records. Success is queue admission, not
    /// durability: the SDK still awaits each document RPC before a terminal.
    fn queue_owned_documents(&mut self) -> Result<(), &'static str> {
        self.refresh_document_routes();
        self.queue_owned_documents_after_refresh()
    }

    fn queue_owned_documents_after_refresh(&mut self) -> Result<(), &'static str> {
        fn collect(root: &dyn Widget, owners: &mut Vec<crate::history_mode::HistoryOwner>) {
            if let Some(owner) = root.history_owner()
                && !owners.iter().any(|known| known.same_owner(&owner))
            {
                owners.push(owner);
            }
            for child in root.children() {
                collect(child, owners);
            }
        }
        // Reconcile can omit a newly unreadable, unroutable or retired owner.
        // Such omissions cannot serve as proof that every live owner was queued.
        let mut current = vec![];
        collect(self.root_widget.as_ref(), &mut current);
        crate::history_mode::capability::visit_framework_history(
            self.root_widget.as_ref(),
            &mut |_, live| collect(live, &mut current),
        );
        if current.is_empty() {
            return Ok(());
        }
        if self.document_routes.witness.is_none() {
            return Err("the admitted document route is unavailable");
        }
        let mut blocked = None;
        if current.iter().any(|owner| {
            !self.document_routes.owners.iter().any(|entry| {
                entry
                    .owners
                    .iter()
                    .any(|installed| installed.same_owner(owner))
            })
        }) {
            blocked = Some("a live document owner has no usable publication route");
        }
        let mut sends = vec![];
        for entry in &mut self.document_routes.owners {
            let Some(owner) = entry.owners.first() else {
                blocked.get_or_insert("a document route has no owner");
                continue;
            };
            if entry.owners.iter().any(|other| !owner.same_document(other)) {
                entry.pending.failure = Some("Multiple different document owners claim this collaboration route. Synchronization is stopped.".into());
                blocked.get_or_insert("document owners conflict on the publication route");
                continue;
            }
            let publication = owner.prepare_publication(
                &self.state.doc,
                &entry.sent_heads,
                entry.needs_snapshot,
                entry.pending.failure.is_some(),
            );
            let (mutation, authorization, heads) = match publication {
                Ok(crate::history_mode::owner::OwnedDocumentPublication::Unchanged) => continue,
                Ok(crate::history_mode::owner::OwnedDocumentPublication::Unfinished) => {
                    blocked.get_or_insert("an owned document has unfinished changes");
                    continue;
                }
                Ok(crate::history_mode::owner::OwnedDocumentPublication::Blocked) => {
                    blocked.get_or_insert("a changed document requires synchronization recovery");
                    continue;
                }
                Ok(crate::history_mode::owner::OwnedDocumentPublication::Changed {
                    mutation,
                    authorization,
                    heads,
                }) => (mutation, authorization, heads),
                Err(_) => {
                    blocked.get_or_insert("an owned document is unreadable");
                    continue;
                }
            };
            let request = CollabRequest::Document {
                mutation,
                authorization,
            };
            sends.push((entry.route.clone(), request, heads));
        }
        for (route, request, heads) in sends {
            let result = self.send_owned_document(route.clone(), request);
            let failure = outbound_failure(&result);
            if result.is_err() {
                blocked.get_or_insert("the document command could not enter the transport queue");
            }
            let Some(entry) = self
                .document_routes
                .owners
                .iter_mut()
                .find(|entry| entry.route == route)
            else {
                continue;
            };
            // Only an accepted change moves the route's frontier; a refused
            // one is offered again on the next publication.
            if result.is_ok() {
                entry.sent_heads = heads;
                entry.needs_snapshot = false;
            }
            if let Some(failure) = failure
                && entry.outbound_failure.as_ref() != failure.as_ref()
            {
                entry.outbound_failure = failure;
                self.layout.layout_dirty = true;
            }
        }
        blocked.map_or(Ok(()), Err)
    }

    /// A local mutation may already exist. Suppress its terminal and retire the
    /// transport; broker disconnect/recovery retains admitted uncertainty.
    pub(super) fn queue_documents_before_terminal(&mut self) -> bool {
        if self.action_publication_failure.is_some() {
            return false;
        }
        match self.queue_owned_documents() {
            Ok(()) => true,
            Err(reason) => {
                self.action_publication_failure = Some(format!(
                    "Document publication blocked: {reason}; action outcome unknown. Reconnect and reconcile before retrying."
                ));
                self.restore_primary_publication_obligations();
                if let Some(room) = self.persistence.room_index.as_mut() {
                    room.advance_outbound(Vec::new());
                }
                self.collaboration.manager.stop_after_publication_failure();
                self.layout.layout_dirty = true;
                false
            }
        }
    }
    #[cfg(test)]
    pub(super) fn drain_document_routes(&mut self) {
        self.refresh_document_routes();
        self.drain_document_routes_after_refresh();
    }

    fn drain_document_routes_after_refresh(&mut self) {
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
                // Remote history is already held by its sender and the owner;
                // republishing it as our Incremental is a pure echo. When every
                // local change had already been queued before this merge, the
                // merged frontier counts as published. Otherwise unsent local
                // changes interleave and the next publication keeps them.
                use automerge::transaction::Transactable as _;
                let (store, checkout) = owner.store_checkout();
                let fully_sent = !entry.needs_snapshot
                    && store
                        .read_checked(&checkout.id, |doc| {
                            doc.pending_ops() == 0
                                && crate::history_mode::sorted_heads(doc.base_heads())
                                    == entry.sent_heads
                        })
                        .unwrap_or(false);
                match owner.merge_remote(mutation) {
                    Ok(Some(applied)) => {
                        entry.pending.pop_applied();
                        changed |= applied;
                        if applied {
                            store.schedule_committed_history(&checkout.id);
                            if fully_sent
                                && let Ok(heads) = store.read_checked(&checkout.id, |doc| {
                                    crate::history_mode::sorted_heads(doc.base_heads())
                                })
                            {
                                entry.sent_heads = heads;
                            }
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

    pub(super) fn pump_document_routes(&mut self) {
        self.refresh_document_routes();
        self.drain_document_routes_after_refresh();
        let _ = self.queue_owned_documents_after_refresh();
    }
    pub(super) fn apply_signed_document_envelope(
        &mut self,
        envelope: &collab_types::SignedOp,
        decrypted_payload: Option<&[u8]>,
    ) {
        // Before the first record checkout the launcher's unqualified route
        // carries room-index discovery, never the editable record document.
        if self.persistence.records.is_some()
            && self.collaboration.room_manager.is_none()
            && self.native_room_route.is_none()
        {
            if let Some(route) = self
                .collaboration
                .manager
                .primary_route()
                .map(|witness| witness.route.clone())
            {
                let _ =
                    self.apply_room_index_document_envelope(&route, envelope, decrypted_payload);
            }
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
            .enumerate()
            .find_map(|(index, entry)| {
                validated_route_payload_with(&entry.route, envelope, decrypted_payload)
                    .map(|payload| (index, entry.route.clone(), payload))
            });
        let primary = validated_route_payload_with(&witness.route, envelope, decrypted_payload)
            .and_then(|payload| {
                let active = !self.document_routes.retired_route(&witness.route)
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
                active.then(|| (witness.route.clone(), payload))
            });
        let (owned, route, payload) = if let Some((index, route, payload)) = owned {
            (Some(index), route, payload)
        } else if let Some((route, payload)) = primary {
            (None, route, payload)
        } else {
            return;
        };
        let mutation = match decode_routed_mutation(&route, payload, decrypted_payload) {
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
        self.drain_document_routes_after_refresh();
    }
}
