//! What a workspace holds, found by words, and what changed in it: the host's answers to a
//! search ([`tilo_actions::remote::ResourceSearch`]) and to a watch of the whole workspace
//! ([`tilo_actions::remote::ChangeFeed`]), asked of it across the wire.
//!
//! The search ranks what each record holds as its app tells search about it: the entries the app
//! publishes to the search index ([`ui::framework::record_index::hosted_entries`]), one per
//! resource of the record (a note by its title and its paragraphs, a file by its name), read in a
//! confined app worker once per change of the record. A record whose app tells search nothing is
//! ranked whole, by its text read app-agnostically from the document ([`crate::search`]). Each hit
//! is answered as the resource the caller's listing names it by, asked through the same catalogue
//! a listing reads, so a caller finds only what it could list and acts on a hit by the path it
//! lists: the note, not the notebook it is in. A record the host holds no keys for has no text
//! here and is not found by it.

use tilo_actions::remote::{ChangePage, Found};
use ui::history_source::feed::RecordKey;
use ui::search::index_publication::IndexEntry;

use super::{
    ActionHostError, Caller, CatalogueQuery, DocumentRoute, Needs, Worker, record_provider,
};
use crate::workspace_records::Reading;
use tilo_relay_protocol::WorkspaceId;

/// The most records one search answers with.
const MOST_FOUND: usize = 20;

/// The most rows of a record's listing read to name one resource a search found in it.
const NAMING_ROWS: u32 = 64;

impl Worker {
    /// What `needle` finds among the records of `workspace_id`, best first, as `caller` lists
    /// them.
    pub(super) fn search(
        &mut self,
        workspace_id: WorkspaceId,
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
        let (docs, searched) = self.searchable(workspace_id, &routes);
        let hits = crate::search::search(&docs, needle, None, docs.len());
        let mut found: Vec<Found> = Vec::new();
        for hit in hits {
            if found.len() >= MOST_FOUND {
                break;
            }
            let (Some(doc), Some(what)) = (docs.get(hit.index), searched.get(hit.index)) else {
                continue;
            };
            let resource = match what {
                Some(entry) => self.named_resource(workspace_id, caller, &doc.route, entry),
                None => self.leading_resource(workspace_id, caller, &doc.route),
            };
            if let Some(resource) = resource
                && !found.iter().any(|held| held.resource == resource.resource)
            {
                found.push(resource);
            }
        }
        Ok(found)
    }

    /// What a search ranks of the records at `routes` the replica holds: each resource their apps
    /// tell search about, with what it says, and each record whose app tells it nothing, whole.
    /// Beside each document, the entry it was made of, or `None` for a record ranked whole.
    fn searchable(
        &mut self,
        workspace_id: WorkspaceId,
        routes: &[DocumentRoute],
    ) -> (Vec<crate::search::SearchDoc>, Vec<Option<IndexEntry>>) {
        let mut docs = Vec::new();
        let mut searched = Vec::new();
        let Some(workspace) = self.workspaces.get_mut(&workspace_id) else {
            return (docs, searched);
        };
        let engine = std::sync::Arc::clone(workspace.peer.engine());
        let mut records = workspace.records.as_mut();
        let readings = &mut self.titles;
        for route in routes.iter().take(crate::search::MOST_SEARCHED) {
            let mut read = |bytes: Vec<u8>| readings.entries(&route.app_id, bytes);
            // Read once per change of the record where the replica's records are followed; read
            // whole where nothing landed yet to follow them by.
            let (entries, changed) = if let Some(records) = records.as_deref_mut() {
                (records.entries(route, read), records.changed(route))
            } else {
                let bytes = engine
                    .record(&route.cluster_id, &route.app_id, &route.record)
                    .ok()
                    .flatten();
                let entries = bytes.and_then(|bytes| read(bytes).ok()).map(Into::into);
                (entries, None)
            };
            let entries = entries.unwrap_or_default();
            for entry in entries.iter() {
                let text = entry
                    .subtitle
                    .iter()
                    .chain(entry.fields.iter().map(|(text, _)| text))
                    .map(String::as_str)
                    .collect::<Vec<&str>>()
                    .join("\n");
                docs.push(crate::search::SearchDoc {
                    route: route.clone(),
                    title: Some(entry.title.clone()),
                    text: text.into(),
                    changed,
                    open: false,
                });
                searched.push(Some(entry.clone()));
            }
            if !entries.is_empty() {
                continue;
            }
            let text = if let Some(records) = records.as_deref_mut() {
                records.text(route, Reading::Plain)
            } else {
                engine
                    .record(&route.cluster_id, &route.app_id, &route.record)
                    .ok()
                    .flatten()
                    .and_then(|bytes| automerge::AutoCommit::load(&bytes).ok())
                    .map(|doc| crate::search::plain_text(&doc).into())
            };
            let Some(text) = text else {
                continue;
            };
            docs.push(crate::search::SearchDoc {
                route: route.clone(),
                title: None,
                text,
                changed,
                open: false,
            });
            searched.push(None);
        }
        (docs, searched)
    }

    /// The resource `entry` is about, in the record at `route`, as `caller`'s listing names it:
    /// its own row, asked for among the rows that mention its id under each resource its id
    /// extends (a note under its notebook), the nearest first, then at the record's top. `None`
    /// for a resource the caller lists nothing of.
    fn named_resource(
        &mut self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        route: &DocumentRoute,
        entry: &IndexEntry,
    ) -> Option<Found> {
        let id = entry.resource.as_str();
        let holders = id
            .match_indices('/')
            .map(|(at, _)| id.get(..at).map(str::to_owned))
            .rev()
            .chain(std::iter::once(None));
        for parent in holders {
            let query = CatalogueQuery {
                provider: Some(record_provider(route)),
                parent_resource: parent,
                needle: entry.resource.clone(),
                max_results: NAMING_ROWS,
                ..CatalogueQuery::default()
            };
            let Ok(page) = self.catalogue_page(workspace_id, caller, &query) else {
                continue;
            };
            if let Some(row) = page.rows.into_iter().find(|row| row.resource.id == id) {
                return Some(Found {
                    resource: row.resource.id,
                    kind: row.resource.kind,
                    label: row.resource.label,
                    open: None,
                    arguments: serde_json::Map::new(),
                    app: Some(route.app_id.clone()),
                    provider: Some(record_provider(route)),
                });
            }
        }
        None
    }

    /// The resource the listing of the record at `route` leads with for `caller`, as a search
    /// answers it: `None` for a record the caller lists nothing of.
    fn leading_resource(
        &mut self,
        workspace_id: WorkspaceId,
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
            provider: Some(record_provider(route)),
        })
    }

    /// The changes to the records of `workspace_id` after `after`, for `caller`.
    pub(super) fn changes(
        &mut self,
        workspace_id: WorkspaceId,
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
