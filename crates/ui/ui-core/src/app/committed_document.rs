//! Merge native committed history into the existing frame/document owner.
use super::App;
use crate::state::StateHistory as _;
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
        // An on-demand record asked for before it was here opens now.
        self.unheld_resource_arrived(&route);
        self.committed_documents.push((route, read));
        self.pump_committed_documents();
    }
    pub(super) fn pump_committed_documents(&mut self) {
        if self.committed_documents.is_empty() {
            return;
        }
        self.refresh_document_routes();
        let pending = std::mem::take(&mut self.committed_documents);
        let mut deferred = false;
        for (route, read) in pending {
            // A read delivered again after it was taken in brings nothing new: its whole
            // document was let go once merged, so taking it again would refuse it and tell the
            // window its history cannot be read here.
            if !read.is_current() || read.was_applied() {
                continue;
            }
            let _phase = tilo_resource::profiling::Phase::start("committed.take");
            // Changes apply only to an owner holding their base; one that
            // does not makes the reader read the document whole.
            let mut lacks_base = false;
            let result = read.with_incoming(|mut incoming| {
                let workspace_route = self
                    .collaboration
                    .workspace_manager
                    .as_ref()
                    .and_then(|manager| manager.primary_route())
                    .or_else(|| {
                        (self.persistence.records.is_some()
                            && self.collaboration.workspace_manager.is_none())
                        .then(|| self.collaboration.manager.primary_route())
                        .flatten()
                    });
                if self.native_workspace_route.as_ref() == Some(&route)
                    || workspace_route.is_some_and(|observation| *observation.route() == route)
                {
                    // Not open yet: try again on the next pump.
                    let Some(workspace) = self.persistence.workspace_index.as_mut() else {
                        return Ok(None);
                    };
                    if workspace.document().pending_ops() != 0 {
                        return Ok(None);
                    }
                    if !read.applies_to(workspace.document()) {
                        lacks_base = true;
                        return Ok(None);
                    }
                    let before = workspace.document().base_heads();
                    match incoming {
                        crate::collaboration::Incoming::Whole(loaded) => {
                            workspace.merge_loaded(loaded)
                        }
                        crate::collaboration::Incoming::Changes(bytes) => {
                            workspace.load_incremental(bytes).map(|_| ())
                        }
                    }
                    .map_err(|e| e.to_string())?;
                    if let Some(records) = self.persistence.records.as_mut() {
                        records.discover_workspace(workspace.document());
                    }
                    return Ok(Some(workspace.document().base_heads() != before));
                }
                if let Some(entry) = self
                    .document_routes
                    .owners
                    .iter_mut()
                    .find(|entry| entry.route == route)
                {
                    // Not installed yet: try again on the next pump.
                    let Some(owner) = entry.owners.first() else {
                        return Ok(None);
                    };
                    if entry.owners.iter().any(|other| !owner.same_document(other)) {
                        return Err("committed document owner ambiguous".into());
                    }
                    let (store, checkout) = owner.store_checkout();
                    // The mutation names history this document must base on.
                    match store.read_checked(&checkout.id, |doc| read.applies_to(doc)) {
                        Ok(true) => {}
                        Ok(false) => {
                            lacks_base = true;
                            return Ok(None);
                        }
                        // Not open yet: try again on the next pump.
                        Err(_) => return Ok(None),
                    }
                    let applied = owner
                        .merge_remote(incoming, &mut self.state.doc)
                        .map_err(|e| e.to_string())?;
                    // Committed history is already held by the owner that
                    // served it, so it is counted as held here too: never
                    // echoed back as this replica's Incremental, and never
                    // covering this replica's own unsent changes.
                    entry.sent.observe(applied.observed);
                    return Ok(applied.changed);
                }
                if self
                    .collaboration
                    .manager
                    .primary_route()
                    .is_some_and(|observation| *observation.route() == route)
                    && !self.document_routes.retired_route(&route)
                    && route.document.as_deref().is_none_or(|record| {
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
                        .merge_remote_committed(incoming.reborrow())
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
                        .merge_remote_committed(incoming)
                        .map_err(|e| e.to_string());
                }
                Ok(None)
            });
            if lacks_base {
                // The change is built on history this replica does not hold (a record it founded
                // its own empty copy of, a peer that was away too long): the record whole is what
                // repairs that, and refusing the changes reads it whole. A change that arrives out
                // of order is repaired the same way.
                tracing::warn!(
                    channel = %route.channel_id(),
                    "committed changes arrived without their history: reading the record whole"
                );
                read.refuse();
                continue;
            }
            match result {
                Ok(Ok(Some(changed))) => {
                    read.applied();
                    if read.is_whole() {
                        self.clear_unreadable_history();
                    }
                    if changed {
                        self.invalidate_widget_resource_cache();
                        self.needs_sync = true;
                        self.layout.layout_dirty = true;
                    }
                }
                Ok(Ok(None)) => {
                    deferred = true;
                    self.committed_documents.push((route, read));
                }
                Ok(Err(error)) | Err(error) => {
                    // The owner cannot read this capture: keeping it would
                    // hold the route's delivery turn on bytes no pass can
                    // merge, so the window would stop receiving and its own
                    // edits stay here. Refuse it, which the reader answers by
                    // reading the record whole (changes) or by keeping this
                    // device's copy and saying so (a whole document), and
                    // keep the window's unfinished local work as it is.
                    tracing::warn!(
                        %error,
                        whole = read.is_whole(),
                        app = %route.app_id,
                        record = route.document.as_deref().unwrap_or("-"),
                        "committed history could not be read here"
                    );
                    read.refuse();
                    if read.is_whole() {
                        self.report_unreadable_history();
                    }
                }
            }
        }
        if deferred {
            // A read waits for what this pass may only now provide: its
            // owner, installed once the window's tree holds the record, or
            // the primary route the connection acknowledges. Nothing else
            // need wake an embedded window once that lands, so the read
            // stayed queued and the window never became ready to publish
            // its actions. Ask for the next pass now.
            let _entry = self.enter_wake_scope();
            crate::wake::WakeHandle::new().notify_events();
        }
    }

    /// The workspace's copy of a record could not be read here: the window
    /// says so, and goes on with the record it holds on this device.
    fn report_unreadable_history(&mut self) {
        self.document_routes.status = Some(crate::i18n::I18nManager::lookup_plain(
            "history-sync-unreadable",
        ));
    }

    /// A whole capture of a record did arrive: what said it could not be read
    /// here is no longer true.
    fn clear_unreadable_history(&mut self) {
        let unreadable = crate::i18n::I18nManager::lookup_plain("history-sync-unreadable");
        if self.document_routes.status.as_deref() == Some(unreadable.as_str()) {
            self.document_routes.status = None;
        }
    }
}
