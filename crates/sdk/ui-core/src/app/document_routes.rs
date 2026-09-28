//! Native owner-route bridge. SDK retains signing, encryption and transport.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::collaboration::RouteSpec;

/// How far an action's document changes got before its terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminalPublication {
    /// Every changed owned record is queued; the terminal may follow.
    Queued,
    /// A changed record's route is still being joined. The terminal waits
    /// for it rather than preceding the change it reports.
    AwaitingRoute,
    /// Publication failed; the transport is retired as uncertain.
    Failed,
}

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
    /// Returns whether a receiving widget may have changed what it declares.
    pub(super) fn forward_session_notification(&mut self, event: &CollabEvent) -> bool {
        // Document traffic enters only the exact custody-checked merge door.
        // A mutable widget callback is not a read-only notification boundary.
        !matches!(
            event,
            CollabEvent::Document { .. }
                | CollabEvent::ScrollbackBatch { .. }
                | CollabEvent::CommittedDocument { .. }
        ) && crate::widget::handle_collab_event(self.root_widget.as_mut(), event)
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
        // One connection carries the record and the room index; a failed
        // publication may have taken either with it.
        if !self.collaboration.manager.document_publication_failed() {
            return;
        }
        self.restore_primary_publication_obligations();
        if let Some(room) = self.persistence.room_index.as_mut() {
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
    /// Answer a late joiner's request for a document this app owns on its
    /// own route — a record it created and others follow — with the whole
    /// document on the next publication. Returns whether `route` was one.
    pub(super) fn answer_owned_document_snapshot(&mut self, route: &RouteSpec) -> bool {
        let Some(entry) = self
            .document_routes
            .owners
            .iter_mut()
            .find(|entry| &entry.route == route)
        else {
            return false;
        };
        entry.needs_snapshot = true;
        self.publish_owned_documents();
        true
    }

    /// Ordinary background/history publication remains best effort. Only a
    /// directed terminal requires the explicit complete-queue result below.
    pub(super) fn publish_owned_documents(&mut self) {
        let _ = self.queue_owned_documents();
    }

    /// Enqueue all changed live owned records. Success is queue admission, not
    /// durability: the SDK still awaits each document RPC before a terminal.
    /// `Ok(false)` leaves a change whose route is still being joined for a
    /// later publication.
    fn queue_owned_documents(&mut self) -> Result<bool, &'static str> {
        self.refresh_document_routes();
        self.queue_owned_documents_after_refresh()
    }

    fn queue_owned_documents_after_refresh(&mut self) -> Result<bool, &'static str> {
        fn collect(root: &dyn Widget, owners: &mut Vec<crate::history_mode::HistoryOwner>) {
            let joined = root.joined_records().map(|joined| joined.owners());
            for owner in root
                .history_owner()
                .into_iter()
                .chain(joined.into_iter().flatten())
            {
                if !owners.iter().any(|known| known.same_owner(&owner)) {
                    owners.push(owner);
                }
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
            return Ok(true);
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
        let mut queued = true;
        for (route, request, heads) in sends {
            let publishable = self
                .collaboration
                .manager
                .document_route_publishable(&route);
            let result = self.send_owned_document(route.clone(), request);
            let failure = outbound_failure(&result);
            if result.is_err() {
                if publishable {
                    blocked
                        .get_or_insert("the document command could not enter the transport queue");
                } else {
                    queued = false;
                }
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
        blocked.map_or(Ok(queued), Err)
    }

    /// A local mutation may already exist. Suppress its terminal and retire the
    /// transport; broker disconnect/recovery retains admitted uncertainty.
    pub(super) fn queue_documents_before_terminal(&mut self) -> TerminalPublication {
        if self.action_publication_failure.is_some() {
            return TerminalPublication::Failed;
        }
        match self.queue_owned_documents() {
            Ok(true) => TerminalPublication::Queued,
            Ok(false) => TerminalPublication::AwaitingRoute,
            Err(reason) => {
                self.fail_terminal_publication(reason);
                TerminalPublication::Failed
            }
        }
    }

    /// A held terminal's route was never joined: retire the transport as for
    /// any other refused publication.
    pub(super) fn fail_awaited_publication(&mut self) {
        self.fail_terminal_publication("a changed document's route was never joined");
    }

    fn fail_terminal_publication(&mut self, reason: &str) {
        tracing::warn!(%reason, "collab: an action's document changes could not be queued");
        self.action_publication_failure = Some(format!(
            "Document publication blocked: {reason}; action outcome unknown. Reconnect and reconcile before retrying."
        ));
        self.restore_primary_publication_obligations();
        if let Some(room) = self.persistence.room_index.as_mut() {
            room.advance_outbound(Vec::new());
        }
        self.collaboration.manager.stop_after_publication_failure();
        self.layout.layout_dirty = true;
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
                    Ok(None) => {
                        tracing::debug!(
                            "collab: a remote change waits for this record's own edits to settle"
                        );
                        break;
                    }
                    Err(error) => {
                        let reason = format!(
                            "Remote document could not be applied: {error}. Synchronization needs recovery."
                        );
                        tracing::warn!(%reason, "collab: a remote change was not applied");
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
                Ok(None) => {
                    tracing::debug!(
                        pending_ops =
                            automerge::transaction::Transactable::pending_ops(&self.state.doc),
                        "collab: a remote change waits for this copy's own edits to settle"
                    );
                    break;
                }
                Err(error) => {
                    let reason = format!(
                        "Remote document could not be applied: {error}. Synchronization needs recovery."
                    );
                    tracing::warn!(%reason, "collab: a remote change was not applied");
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
            self.presence.follow_after_sync |= self.presence.followed_peer.is_some();
        }
    }
    /// A change on a route a widget follows: somebody else's record, merged
    /// into the widget's replica. Only the record's own author may have made
    /// it. Returns whether the envelope was on a followed route at all.
    fn apply_followed_document_envelope(
        &mut self,
        envelope: &collab_types::SignedOp,
        decrypted_payload: Option<&[u8]>,
    ) -> bool {
        let Some((follower, payload)) =
            self.document_routes.followers.iter().find_map(|follower| {
                validated_route_payload_with(&follower.route, envelope, decrypted_payload)
                    .map(|payload| (follower, payload))
            })
        else {
            return false;
        };
        let record = follower.route.record.clone().unwrap_or_default();
        if collab_types::check_document_route(&record, envelope.op_class, &envelope.author).is_err()
            || !collab_types::is_comment_record_of(&record, &envelope.author)
        {
            tracing::warn!(
                record,
                "collab: refused a followed record's change from somebody else"
            );
            return true;
        }
        let mutation = match decode_routed_mutation(&follower.route, payload, decrypted_payload) {
            Ok(mutation) => mutation,
            Err(error) => {
                tracing::warn!(%error, "collab: invalid followed document mutation");
                return true;
            }
        };
        let merged = follower
            .replica
            .try_borrow_mut()
            .map_or(Ok(false), |mut replica| {
                crate::history_mode::routes::merge_committed(&mut replica, &mutation)
            });
        match merged {
            Ok(true) => {
                self.invalidate_widget_resource_cache();
                self.needs_sync = true;
                self.layout.layout_dirty = true;
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, record, "collab: a followed record could not be merged")
            }
        }
        true
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
        // A followed record's route names that record; it is never the
        // unqualified room-index route nor the primary document.
        self.refresh_document_routes();
        if self.apply_followed_document_envelope(envelope, decrypted_payload) {
            return;
        }
        // The launcher's unqualified route carries room-index discovery, never
        // the editable record document: it is the primary route until the
        // first record checkout, and joined beside the record's after it.
        if self.persistence.records.is_some() {
            let joined_room_route = self
                .native_room_route
                .clone()
                .or_else(|| self.collaboration.room_route.clone());
            let room_route = joined_room_route.clone().or_else(|| {
                self.collaboration
                    .manager
                    .primary_route()
                    .map(|witness| witness.route.clone())
                    .filter(|route| route.record.is_none())
            });
            let on_room_route = room_route.as_ref().is_some_and(|route| {
                validated_route_payload_with(route, envelope, decrypted_payload).is_some()
            });
            if on_room_route || joined_room_route.is_none() {
                if let Some(route) = room_route {
                    let _ = self.apply_room_index_document_envelope(
                        &route,
                        envelope,
                        decrypted_payload,
                    );
                }
                return;
            }
        }
        let Some(witness) = self.document_routes.witness.as_ref() else {
            tracing::debug!(
                seq = envelope.seq,
                "collab: a document change arrived before this app chose its document"
            );
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
            tracing::debug!(
                seq = envelope.seq,
                "collab: a document change for a route this app does not hold"
            );
            return;
        };
        // Brokers enforce the class rule; the receiver holds it too, and
        // takes no comment-class change on a document route that is not the
        // author's own comment record.
        let record = route.record.clone().unwrap_or_default();
        if collab_types::check_document_route(&record, envelope.op_class, &envelope.author).is_err()
            || (record.is_empty() && envelope.op_class == collab_types::OpClass::Comment)
        {
            tracing::warn!(record, class = ?envelope.op_class, "collab: refused a document change whose class does not fit its route");
            return;
        }
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
