//! What a workspace holds, found by words, and what changed in it: the host's answers to a
//! search ([`tilo_actions::remote::ResourceSearch`]) and to a watch of the whole workspace
//! ([`tilo_actions::remote::ChangeFeed`]), asked of it across the wire.
//!
//! The search is the one the host's own agents plan with ([`crate::search`]): every record the
//! replica holds, ranked by what it says (its text read app-agnostically from the document) and
//! its app. Each record found is answered as the resource its listing leads with for the caller,
//! asked through the same catalogue a listing reads, so a caller finds only what it could list
//! and acts on a hit by the path it lists. A record the host holds no keys for has no text here
//! and is not found by it.

use tilo_actions::remote::{ChangePage, Found};
use ui::history_source::feed::RecordKey;

use super::{
    ActionHostError, Caller, CatalogueQuery, DocumentRoute, Needs, Worker, record_provider,
};
use crate::workspace_records::Reading;

/// The most records one search answers with.
const MOST_FOUND: usize = 20;

impl Worker {
    /// What `needle` finds among the records of `workspace_id`, best first, as `caller` lists
    /// them.
    pub(super) fn search(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        needle: &str,
    ) -> Result<Vec<Found>, ActionHostError> {
        if !self.admits_member(workspace_id, caller) {
            return Err(ActionHostError::NotPermitted);
        }
        let state = self.refresh(workspace_id, Needs::Workspace)?;
        let Some(cluster_id) = state.workspace.clone() else {
            return Ok(Vec::new());
        };
        let routes = self.records(&state, &cluster_id);
        let docs = self.searchable(workspace_id, &routes);
        let hits = crate::search::search(&docs, needle, None, MOST_FOUND);
        let mut found = Vec::new();
        for hit in hits {
            let Some(doc) = docs.get(hit.index) else {
                continue;
            };
            if let Some(resource) = self.leading_resource(workspace_id, caller, &doc.route) {
                found.push(resource);
            }
        }
        Ok(found)
    }

    /// The records of `routes` the replica holds, with their text, for a search.
    fn searchable(
        &mut self,
        workspace_id: u64,
        routes: &[DocumentRoute],
    ) -> Vec<crate::search::SearchDoc> {
        let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
            return Vec::new();
        };
        let engine = std::sync::Arc::clone(workspace.peer.engine());
        let mut records = workspace.records.as_mut();
        routes
            .iter()
            .take(crate::search::MOST_SEARCHED)
            .filter_map(|route| {
                // Read once per change of the record where the replica's records are followed;
                // read whole where nothing landed yet to follow them by.
                let (text, changed) = if let Some(records) = records.as_deref_mut() {
                    (records.text(route, Reading::Plain)?, records.changed(route))
                } else {
                    let bytes = engine
                        .record(&route.cluster_id, &route.app_id, &route.record)
                        .ok()??;
                    let doc = automerge::AutoCommit::load(&bytes).ok()?;
                    (crate::search::plain_text(&doc).into(), None)
                };
                Some(crate::search::SearchDoc {
                    route: route.clone(),
                    title: None,
                    text,
                    changed,
                    open: false,
                })
            })
            .collect()
    }

    /// The resource the listing of the record at `route` leads with for `caller`, as a search
    /// answers it: `None` for a record the caller lists nothing of.
    fn leading_resource(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        route: &DocumentRoute,
    ) -> Option<Found> {
        let query = CatalogueQuery {
            provider: Some(record_provider(route)),
            max_results: 1,
            ..CatalogueQuery::default()
        };
        let page = self.catalogue_page(workspace_id, caller, &query).ok()?;
        let row = page.rows.into_iter().next()?;
        Some(Found {
            resource: row.resource.id,
            kind: row.resource.kind,
            label: row.resource.label,
            open: None,
            arguments: serde_json::Map::new(),
            app: Some(route.app_id.clone()),
        })
    }

    /// The changes to the records of `workspace_id` after `after`, for `caller`.
    pub(super) fn changes(
        &mut self,
        workspace_id: u64,
        caller: &Caller,
        after: Option<&str>,
    ) -> Result<ChangePage, ActionHostError> {
        if !self.admits_member(workspace_id, caller) {
            return Err(ActionHostError::NotPermitted);
        }
        let state = self.refresh(workspace_id, Needs::Workspace)?;
        // Nothing written in the workspace yet: no records, and its first ones are news.
        let routes = match state.workspace.clone() {
            Some(cluster_id) => self.records(&state, &cluster_id),
            None => Vec::new(),
        };
        let records: Vec<RecordKey> = routes
            .iter()
            .map(|route| (route.app_id.clone(), route.record.clone()))
            .collect();
        let cluster_id = state.workspace.clone().unwrap_or_default();
        let engine = std::sync::Arc::clone(&state.engine);
        let log = &mut self.membership(workspace_id)?.changes;
        log.take_in(
            &records,
            |(app, record)| {
                engine
                    .record_revision(&cluster_id, app, record)
                    .ok()
                    .flatten()
            },
            |(app, record)| engine.record(&cluster_id, app, record).ok().flatten(),
            &ui::history_source::sentence::ThreadWords,
        );
        Ok(log.page(after))
    }
}
