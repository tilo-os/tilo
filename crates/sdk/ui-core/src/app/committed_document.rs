//! Merge native committed history into the existing frame/document owner.
use super::App;
use automerge::transaction::Transactable;
impl App {
    pub(super) fn queue_committed_document(
        &mut self,
        route: crate::collaboration::RouteSpec,
        read: crate::collaboration::CommittedDocument,
    ) {
        if self.committed_documents.len() >= 64 {
            return;
        }
        self.committed_documents.push((route, read));
        self.pump_committed_documents();
    }
    pub(super) fn pump_committed_documents(&mut self) {
        if self.committed_documents.is_empty() {
            return;
        }
        self.refresh_document_routes();
        let pending = std::mem::take(&mut self.committed_documents);
        for (route, read) in pending {
            if !read.is_current() {
                continue;
            }
            // Changes apply only to an owner holding their base; one that
            // does not makes the reader read the document whole.
            let mut lacks_base = false;
            let result = read.with_mutation(|mutation| {
                let room_route = self
                    .collaboration
                    .room_manager
                    .as_ref()
                    .and_then(|manager| manager.primary_route())
                    .or_else(|| {
                        (self.persistence.records.is_some()
                            && self.collaboration.room_manager.is_none())
                        .then(|| self.collaboration.manager.primary_route())
                        .flatten()
                    });
                if self.native_room_route.as_ref() == Some(&route)
                    || room_route.is_some_and(|witness| witness.route == route)
                {
                    let room = self
                        .persistence
                        .room_index
                        .as_mut()
                        .ok_or("committed room owner unavailable".to_string())?;
                    if room.document().pending_ops() != 0 {
                        return Ok(None);
                    }
                    if !read.applies_to(room.document()) {
                        lacks_base = true;
                        return Ok(None);
                    }
                    let before = room.document().base_heads();
                    match mutation {
                        collab_types::DocumentMutation::Snapshot(bytes) => {
                            room.merge_snapshot(bytes)
                        }
                        collab_types::DocumentMutation::Incremental(bytes) => {
                            room.load_incremental(bytes).map(|_| ())
                        }
                    }
                    .map_err(|e| e.to_string())?;
                    if let Some(records) = self.persistence.records.as_mut() {
                        records.discover_room(room.document());
                    }
                    return Ok(Some(room.document().base_heads() != before));
                }
                if let Some(entry) = self
                    .document_routes
                    .owners
                    .iter_mut()
                    .find(|entry| entry.route == route)
                {
                    let owner = entry
                        .owners
                        .first()
                        .ok_or("committed document owner missing")?;
                    if entry.owners.iter().any(|other| !owner.same_document(other)) {
                        return Err("committed document owner ambiguous".into());
                    }
                    let (store, checkout) = owner.store_checkout();
                    // The document's heads before this merge, when it has no
                    // unfinished edit.
                    let before = match store.read_checked(&checkout.id, |doc| {
                        read.applies_to(doc).then(|| {
                            (doc.pending_ops() == 0)
                                .then(|| crate::history_mode::sorted_heads(doc.base_heads()))
                        })
                    }) {
                        Ok(Some(before)) => before,
                        Ok(None) => {
                            lacks_base = true;
                            return Ok(None);
                        }
                        // Busy or not open yet: try again on the next pump.
                        Err(_) => return Ok(None),
                    };
                    // The same record published as the primary document.
                    let mut bound = self
                        .actions
                        .bound_record_sync
                        .as_mut()
                        .filter(|sync| sync.publishes(store, checkout));
                    // Whether every local change was already published on a
                    // route before this merge: then the merged frontier
                    // counts as published there too, exactly as for a live
                    // remote change.
                    let route_sent = before
                        .as_ref()
                        .is_some_and(|before| !entry.needs_snapshot && *before == entry.sent_heads);
                    let primary_sent = before.as_ref().is_some_and(|before| {
                        bound
                            .as_ref()
                            .is_some_and(|sync| sync.published_through(before))
                    });
                    let mut applied = owner.merge_remote(mutation).map_err(|e| e.to_string())?;
                    // A merge is an action boundary, as an undo or a save
                    // is: the word still being typed becomes its own step,
                    // named by the action typing it. Waiting for the
                    // person's next boundary instead held this history, and
                    // every action queued behind its delivery turn, for as
                    // long as the window sat idle. A step no action names
                    // stays open: committed unnamed, the identity service
                    // would refuse it.
                    if applied.is_none() && owner.close_named_step(&mut self.state.doc) {
                        applied = owner.merge_remote(mutation).map_err(|e| e.to_string())?;
                    }
                    // Committed history is already held by the owner that
                    // served it; republishing it as this replica's
                    // Incremental would be a pure echo, one more owner
                    // commit per remote edit.
                    if applied == Some(true)
                        && (route_sent || primary_sent)
                        && let Ok(heads) = store.read_checked(&checkout.id, |doc| {
                            crate::history_mode::sorted_heads(doc.base_heads())
                        })
                    {
                        if primary_sent && let Some(sync) = bound.as_mut() {
                            sync.observe_published(heads.clone());
                        }
                        if route_sent {
                            entry.sent_heads = heads;
                        }
                    }
                    return Ok(applied);
                }
                if self
                    .collaboration
                    .manager
                    .primary_route()
                    .is_some_and(|witness| witness.route == route)
                    && !self.document_routes.retired_route(&route)
                    && route.record.as_deref().is_none_or(|record| {
                        self.persistence
                            .records
                            .as_ref()
                            .and_then(|records| records.active())
                            .is_some_and(|active| active.as_str() == record)
                    })
                {
                    if !read.applies_to(&self.state.doc) {
                        lacks_base = true;
                        return Ok(None);
                    }
                    let merged = self
                        .state
                        .merge_remote_committed(mutation)
                        .map_err(|e| e.to_string())?;
                    if merged.is_some() {
                        return Ok(merged);
                    }
                    // Unfinished typing is closed as the step it is, as for
                    // an owned document above, rather than deferring the
                    // merge until the next input event.
                    self.commit_action();
                    return self
                        .state
                        .merge_remote_committed(mutation)
                        .map_err(|e| e.to_string());
                }
                Ok(None)
            });
            if lacks_base {
                #[cfg(any(test, feature = "test-support"))]
                tracing::warn!(
                    ?route,
                    "committed changes refused: the owner lacks their base"
                );
                read.refuse();
                continue;
            }
            match result {
                Ok(Ok(Some(changed))) => {
                    read.applied();
                    if changed {
                        self.invalidate_widget_resource_cache();
                        self.needs_sync = true;
                        self.layout.layout_dirty = true;
                    }
                }
                Ok(Ok(None)) => self.committed_documents.push((route, read)),
                Ok(Err(error)) | Err(error) => {
                    tracing::warn!(%error,"committed history remains blocked at document owner");
                    // Preserve unfinished local work and this exact immutable
                    // capture. Never acknowledge a failed or deferred merge.
                    self.committed_documents.push((route, read));
                }
            }
        }
    }
}
