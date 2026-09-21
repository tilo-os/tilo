//! Native owner-route bridge. SDK retains signing, encryption and transport.
#[allow(
    clippy::wildcard_imports,
    reason = "private implementation module shares its enclosing host types"
)]
use super::*;
use crate::collaboration::RouteSpec;

#[cfg(test)]
thread_local! {
    pub(crate) static DOCUMENT_ROUTE_REFRESHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn matches_route(route: &RouteSpec, envelope: &collab_types::SignedOp) -> bool {
    validated_route_payload(route, envelope).is_some()
}

pub(super) fn validated_route_payload(
    route: &RouteSpec,
    envelope: &collab_types::SignedOp,
) -> Option<collab_types::VersionedRoutedDocumentPayload> {
    collab_types::validate_document_payload_route_versioned(
        &route.app_id,
        &route.cluster_id.to_string(),
        route.record.as_deref().unwrap_or(""),
        envelope,
    )
    .ok()
}

pub(super) fn decode_routed_mutation(
    route: &RouteSpec,
    payload: collab_types::VersionedRoutedDocumentPayload,
    decrypted_payload: Option<&[u8]>,
) -> Result<collab_types::DocumentMutation, collab_types::DocumentMutationError> {
    match payload {
        collab_types::VersionedRoutedDocumentPayload::Cleartext(decoded) => Ok(decoded.mutation),
        collab_types::VersionedRoutedDocumentPayload::Encrypted { channel, .. } => {
            let decrypted_payload =
                decrypted_payload.ok_or(collab_types::DocumentMutationError::RouteMismatch)?;
            let decoded = collab_types::DocumentMutation::decode_versioned(decrypted_payload)?;
            let record = route.record.as_deref().unwrap_or("");
            if collab_types::document_channel_id_for_version(
                decoded.version,
                &route.app_id,
                &route.cluster_id.to_string(),
                record,
            ) != channel
            {
                return Err(collab_types::DocumentMutationError::RouteMismatch);
            }
            Ok(decoded.mutation)
        }
    }
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
        self.action_publication_failure.as_deref().or(self
            .document_routes
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
            }))
    }
    pub(super) fn refresh_document_routes(&mut self) {
        #[cfg(test)]
        DOCUMENT_ROUTE_REFRESHES.set(DOCUMENT_ROUTE_REFRESHES.get() + 1);
        let before = self.document_sync_status().map(str::to_owned);
        let witness = self.bound_broker_route().cloned();
        let joins = self.document_routes.reconcile(
            self.root_widget.as_ref(),
            witness,
            self.persistence.autosave_generation,
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
    fn send_owned_document(&mut self, route: RouteSpec, request: CollabRequest) -> bool {
        #[cfg(test)]
        let observation = (route.clone(), request.clone());
        if !self.collaboration.manager.try_send_to_route(route, request) {
            return false;
        }
        #[cfg(test)]
        self.document_routes.sent.push(observation);
        true
    }

    /// Ordinary background/history publication remains best effort. Only a
    /// directed terminal requires the explicit complete-queue result below.
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
        self.root_widget
            .visit_framework_history(&mut |_, live| collect(live, &mut current));
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
            let (mut document, heads) = match publication {
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
                    document,
                    heads,
                }) => (document, heads),
                Err(_) => {
                    blocked.get_or_insert("an owned document is unreadable");
                    continue;
                }
            };
            // Clone serialization leaves the canonical writer and every other
            // subscriber's incremental save cursor intact.
            let request = if entry.needs_snapshot {
                CollabRequest::document_snapshot(document.save())
            } else {
                CollabRequest::document_incremental(document.save_after(&entry.sent_heads))
            };
            sends.push((entry.route.clone(), request, heads));
        }
        for (route, request, heads) in sends {
            if self.send_owned_document(route.clone(), request) {
                if let Some(entry) = self
                    .document_routes
                    .owners
                    .iter_mut()
                    .find(|entry| entry.route == route)
                {
                    entry.sent_heads = heads;
                    entry.needs_snapshot = false;
                }
            } else {
                blocked.get_or_insert("the document command could not enter the transport queue");
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
        if self.persistence.records.is_some() && self.collaboration.room_manager.is_none() {
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
                validated_route_payload(&entry.route, envelope)
                    .map(|payload| (index, entry.route.clone(), payload))
            });
        let primary = validated_route_payload(&witness.route, envelope).and_then(|payload| {
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

#[cfg(test)]
mod legacy_route_tests {
    use super::*;

    #[test]
    fn authenticated_legacy_document_route_is_not_rewritten_to_current() {
        let route = RouteSpec::record(
            "dev.qos.calculator",
            uuid::Uuid::new_v4(),
            "tab:5f7c000000000013",
        );
        let workspace = route.cluster_id.to_string();
        let record = route.record.as_deref().expect("record route");
        let version = collab_types::DocumentWireVersion::QosV1;
        let envelope = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Incremental(b"retained legacy edit".to_vec())
                .encode_for(version)
                .expect("legacy mutation"),
            collab_types::document_artifact_id_for_version(
                version,
                &route.app_id,
                &workspace,
                record,
            ),
            collab_types::Ed25519PublicKey::from_bytes([7; 32]),
            collab_types::document_attestation_id_for_version(
                version,
                &route.app_id,
                &workspace,
                record,
            ),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        );
        assert!(matches_route(&route, &envelope));

        let mut mixed = envelope;
        mixed.attestation_id =
            collab_types::document_attestation_id(&route.app_id, &workspace, record);
        assert!(!matches_route(&route, &mixed));
        assert!(!matches_route(
            &RouteSpec::record(route.app_id.clone(), uuid::Uuid::new_v4(), record),
            &mixed,
        ));

        let room_index = RouteSpec::app("dev.qos.calculator", uuid::Uuid::new_v4());
        let workspace = room_index.cluster_id.to_string();
        let room_index_envelope = collab_types::SignedOp::new_unsigned(
            collab_types::DocumentMutation::Snapshot(b"legacy room index".to_vec())
                .encode_for(version)
                .expect("legacy room-index mutation"),
            collab_types::document_artifact_id_for_version(
                version,
                &room_index.app_id,
                &workspace,
                "",
            ),
            collab_types::Ed25519PublicKey::from_bytes([7; 32]),
            collab_types::document_attestation_id_for_version(
                version,
                &room_index.app_id,
                &workspace,
                "",
            ),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            1,
        );
        assert!(
            matches_route(&room_index, &room_index_envelope),
            "the acknowledged unqualified route binds a retained room index"
        );
        assert!(
            !matches_route(&route, &room_index_envelope),
            "a room-index envelope cannot enter a record route"
        );
    }

    #[test]
    fn decrypted_legacy_mutation_must_match_its_signed_mls_namespace() {
        let route = RouteSpec::record(
            "dev.qos.calculator",
            uuid::Uuid::new_v4(),
            "tab:5f7c000000000013",
        );
        let workspace = route.cluster_id.to_string();
        let record = route.record.as_deref().expect("record route");
        let legacy = collab_types::DocumentWireVersion::QosV1;
        let channel = collab_types::document_channel_id_for_version(
            legacy,
            &route.app_id,
            &workspace,
            record,
        );
        let envelope = collab_types::SignedOp::new_unsigned_with_encryption(
            vec![1],
            collab_types::document_artifact_id_for_version(
                legacy,
                &route.app_id,
                &workspace,
                record,
            ),
            collab_types::Ed25519PublicKey::from_bytes([7; 32]),
            collab_types::document_attestation_id_for_version(
                legacy,
                &route.app_id,
                &workspace,
                record,
            ),
            collab_types::OpClass::Splice,
            std::time::SystemTime::UNIX_EPOCH,
            1,
            collab_types::EncryptionMetadata { epoch: 3, channel },
        );
        let payload = validated_route_payload(&route, &envelope).expect("legacy MLS route");
        let legacy_clear = collab_types::DocumentMutation::Incremental(b"legacy".to_vec())
            .encode_for(legacy)
            .expect("legacy cleartext");
        assert_eq!(
            decode_routed_mutation(&route, payload.clone(), Some(&legacy_clear)),
            Ok(collab_types::DocumentMutation::Incremental(
                b"legacy".to_vec()
            ))
        );
        let current_clear = collab_types::DocumentMutation::Incremental(b"current".to_vec())
            .encode()
            .expect("current cleartext");
        assert_eq!(
            decode_routed_mutation(&route, payload, Some(&current_clear)),
            Err(collab_types::DocumentMutationError::RouteMismatch),
            "decrypted current payload cannot cross a legacy signed MLS channel"
        );
    }
}
