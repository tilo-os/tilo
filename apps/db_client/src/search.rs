//! Retained bounded global-search index for database resources.
//!
//! Catalog discovery happens when the live schema or shared document changes,
//! never on each palette keystroke. The `SearchCatalog` owns matching and
//! continuation cursors, so very large schemas remain pageable.

use std::sync::{Arc, RwLock};

use automerge::AutoCommit;
use ui::qos_actions::ActionAddress;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::app::catalog_resource_id;
use crate::doc::SavedQueriesDoc;
use crate::session::Session;

const KIND: &str = "Database";

#[derive(Default)]
struct IndexState {
    revision: Option<String>,
    catalog: Option<SearchCatalog>,
}

/// A cheap-to-clone handle to the current immutable search catalogue.
#[derive(Clone, Default)]
pub struct DbSearchIndex {
    state: Arc<RwLock<IndexState>>,
}

impl DbSearchIndex {
    pub fn sync(&self, session: &Session, doc: &mut AutoCommit) {
        let revision = revision(session, doc);
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.revision.as_deref() == Some(&revision) && state.catalog.is_some() {
            return;
        }
        drop(state);

        let catalog = SearchCatalog::new(KIND);
        for connection in &session.connections {
            let id = format!("connection:{}", connection.id);
            let result = SearchResult::for_action(
                KIND,
                &connection.name,
                ActionAddress::new(&id, "connect"),
            )
            .with_subtitle(connection.subtitle());
            let _ = catalog.upsert(
                &id,
                format!("{} {}", connection.name, connection.subtitle()),
                result,
            );
        }
        if let Some(connection) = session.active_connection() {
            let connection_id = connection.id.to_string();
            for schema in &session.schema.schemas {
                let schema_id = catalog_resource_id("schema", &[&connection_id, &schema.name]);
                for relation in &schema.tables {
                    let relation_id = catalog_resource_id(
                        "relation",
                        &[&connection_id, &schema.name, &relation.name],
                    );
                    let result = SearchResult::for_action(
                        KIND,
                        &relation.name,
                        ActionAddress::new(&relation_id, "browse"),
                    )
                    .with_subtitle(format!("{}.{}", schema.name, relation.name));
                    let _ = catalog.upsert(
                        &relation_id,
                        format!("{} {} {}", schema.name, relation.name, relation.kind as u8),
                        result,
                    );
                    // Columns resolve through their owning relation's Browse
                    // action: choosing one navigates to the real table instead
                    // of promising a no-op column action.
                    for column in &relation.columns {
                        let column_id = catalog_resource_id(
                            "column",
                            &[&connection_id, &schema.name, &relation.name, &column.name],
                        );
                        let result = SearchResult::for_action(
                            KIND,
                            &column.name,
                            ActionAddress::new(&relation_id, "browse"),
                        )
                        .with_subtitle(format!(
                            "{}.{} · {}",
                            schema.name, relation.name, column.decl_type
                        ));
                        let _ = catalog.upsert(
                            &column_id,
                            format!(
                                "{} {} {} {}",
                                schema.name, relation.name, column.name, column.decl_type
                            ),
                            result,
                        );
                    }
                }
                let _ = schema_id;
            }
        }
        for query in SavedQueriesDoc::read(doc) {
            let id = format!("saved-query:{}", query.id);
            let result =
                SearchResult::for_action(KIND, &query.name, ActionAddress::new(&id, "run"))
                    .with_subtitle("Collaborative saved query");
            let _ = catalog.upsert(&id, format!("{} {}", query.name, query.sql), result);
        }
        let mut state = self
            .state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.revision = Some(revision);
        state.catalog = Some(catalog);
    }

    pub fn provider(&self) -> DbSearchProvider {
        DbSearchProvider {
            index: self.clone(),
        }
    }
}

pub struct DbSearchProvider {
    index: DbSearchIndex,
}

impl SearchProvider for DbSearchProvider {
    fn name(&self) -> &'static str {
        KIND
    }
    fn action_directory_generation(&self) -> Option<u64> {
        self.index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .as_ref()
            .map(|catalog| catalog.stats().generation)
    }
    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .as_ref()
            .is_some_and(|catalog| catalog.cursor_is_current(cursor))
    }
    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }
    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.search_page(query, None, limit, limit).results
    }
    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .as_ref()
            .map_or_else(
                || SearchCatalog::new(KIND).query_page(query, cursor, work_budget, result_limit),
                |catalog| catalog.query_page(query, cursor, work_budget, result_limit),
            )
    }
}

fn revision(session: &Session, doc: &mut AutoCommit) -> String {
    let mut parts = vec![format!("heads:{:?}", doc.get_heads())];
    for schema in &session.schema.schemas {
        parts.push(schema.name.clone());
        for relation in &schema.tables {
            parts.push(format!(
                "{}:{}:{}",
                relation.name,
                relation.columns.len(),
                relation.kind as u8
            ));
        }
    }
    parts.join("\0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_relations_are_findable_through_bounded_catalog_pages() {
        let session = Session::with_sample();
        let mut doc = AutoCommit::new();
        let index = DbSearchIndex::default();
        index.sync(&session, &mut doc);
        let provider = index.provider();
        let page = provider.search_page("albums", None, 1, 1);
        assert!(page.inspected <= 1);
        let hit = page.results.first().expect("relation search hit");
        assert_eq!(hit.title, "albums");
        assert_eq!(
            hit.action_address().expect("canonical address").action,
            "browse"
        );
    }

    #[test]
    fn changed_catalog_invalidates_old_search_cursors() {
        let mut session = Session::with_sample();
        let mut doc = AutoCommit::new();
        let index = DbSearchIndex::default();
        index.sync(&session, &mut doc);
        let provider = index.provider();
        let first = provider.search_page("", None, 1, 1);
        let cursor = first.next.expect("large catalog has a continuation");
        let mut relation = session.schema.schemas[0].tables[0].clone();
        relation.name = "cursor-change".into();
        session.schema.schemas[0].tables.push(relation);
        index.sync(&session, &mut doc);
        assert!(!provider.action_directory_cursor_is_current(&cursor));
    }
}
