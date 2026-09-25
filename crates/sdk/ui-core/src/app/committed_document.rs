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
                    .iter()
                    .find(|entry| entry.route == route)
                {
                    let owner = entry
                        .owners
                        .first()
                        .ok_or("committed document owner missing")?;
                    if entry.owners.iter().any(|other| !owner.same_document(other)) {
                        return Err("committed document owner ambiguous".into());
                    }
                    return owner.merge_remote(mutation).map_err(|e| e.to_string());
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
                    return self
                        .state
                        .merge_remote_committed(mutation)
                        .map_err(|e| e.to_string());
                }
                Ok(None)
            });
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
