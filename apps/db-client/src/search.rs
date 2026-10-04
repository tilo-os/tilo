//! Retained bounded global-search index for database resources.
//!
//! Catalog discovery happens when the live schema or shared document changes,
//! never on each palette keystroke. The `SearchCatalog` owns matching and
//! continuation cursors, so very large schemas remain pageable.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchResult};

use tilo_actions::{ActionSpec, OperationKind, Resource, RetryPolicy};
use ui::tilo_actions::ActionAddress;

use crate::app::{DB_OPERATION_TYPE, DbClientOperation};
use crate::app::{catalog_resource_id, job};
use crate::session::Session;

const KIND: &str = "Database";

#[derive(Default)]
struct IndexState {
    revision: Option<String>,
    catalog: Option<SearchCatalog>,
    resources: HashMap<String, Resource>,
    /// The most recent rejected rebuild. Keep the last known-good snapshot
    /// available, but never make a failed index refresh invisible to the app.
    last_error: Option<String>,
}

/// A cheap-to-clone handle to the current immutable search catalogue.
#[derive(Clone, Default)]
pub struct DbSearchIndex {
    state: Arc<RwLock<IndexState>>,
}

impl DbSearchIndex {
    /// Refresh the retained catalogue from the live session.
    ///
    /// # Errors
    ///
    /// Returns an error when the generated rows violate the canonical search
    /// catalogue contract. The previous valid snapshot remains available.
    pub fn sync(&self, session: &Session) -> Result<(), String> {
        let revision = revision(session);
        let state = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.revision.as_deref() == Some(&revision) && state.catalog.is_some() {
            return Ok(());
        }
        let catalog = state
            .catalog
            .clone()
            .unwrap_or_else(|| SearchCatalog::new(KIND));
        drop(state);

        let mut rows = Vec::new();
        let mut resources = HashMap::new();
        index_active_catalog(session, &mut rows, &mut resources);
        apply_catalog_snapshot(&self.state, revision, catalog, rows, resources)
    }
}

fn index_active_catalog(
    session: &Session,
    rows: &mut Vec<(String, String, SearchResult)>,
    resources: &mut HashMap<String, Resource>,
) {
    let Some(connection) = session.active_connection().filter(|_| session.connected) else {
        return;
    };
    let connection_id = connection.id.to_string();
    index_database_and_schema(session, connection, &connection_id, rows, resources);
    index_current_result(session, &connection_id, rows, resources);
    index_transactions(&connection_id, rows, resources);
    index_query_history(session, connection, &connection_id, rows, resources);
}

fn index_database_and_schema(
    session: &Session,
    connection: &crate::model::ConnectionProfile,
    connection_id: &str,
    rows: &mut Vec<(String, String, SearchResult)>,
    resources: &mut HashMap<String, Resource>,
) {
    let database_name = if connection.database.is_empty() {
        "Connected database".to_string()
    } else {
        connection.database.clone()
    };
    let database_id = catalog_resource_id("database", &[connection_id, &database_name]);
    let mut database = Resource::new(&database_id, "database.database", &database_name);
    if let Ok(action) = ActionSpec::operation(
        "focus",
        "Show database",
        OperationKind::View,
        DB_OPERATION_TYPE,
        &DbClientOperation::FocusCatalog {
            connection_id: connection_id.to_string(),
            schema: None,
        },
    ) {
        database.actions.push(action);
    }
    index_resource(
        rows,
        resources,
        database,
        "focus",
        database_name.clone(),
        SearchResult::for_action(
            KIND,
            &database_name,
            ActionAddress::new(&database_id, "focus"),
        )
        .with_subtitle("Active database"),
    );
    for schema in &session.schema.schemas {
        let schema_id = catalog_resource_id("schema", &[connection_id, &schema.name]);
        let mut schema_resource = Resource::new(&schema_id, "database.schema", &schema.name);
        if let Ok(action) = ActionSpec::operation(
            "focus",
            "Show schema",
            OperationKind::View,
            DB_OPERATION_TYPE,
            &DbClientOperation::FocusCatalog {
                connection_id: connection_id.to_string(),
                schema: Some(schema.name.clone()),
            },
        ) {
            schema_resource.actions.push(action);
        }
        index_resource(
            rows,
            resources,
            schema_resource,
            "focus",
            schema.name.clone(),
            SearchResult::for_action(KIND, &schema.name, ActionAddress::new(&schema_id, "focus"))
                .with_subtitle(format!("Schema in {database_name}")),
        );
        for relation in &schema.tables {
            index_relation(connection_id, schema, relation, rows, resources);
        }
    }
}

fn index_relation(
    connection_id: &str,
    schema: &crate::model::SchemaInfo,
    relation: &crate::model::TableInfo,
    rows: &mut Vec<(String, String, SearchResult)>,
    resources: &mut HashMap<String, Resource>,
) {
    let relation_id =
        catalog_resource_id("relation", &[connection_id, &schema.name, &relation.name]);
    let result = SearchResult::for_action(
        KIND,
        &relation.name,
        ActionAddress::new(&relation_id, "browse"),
    )
    .with_subtitle(format!("{}.{}", schema.name, relation.name));
    let mut resource = Resource::new(
        &relation_id,
        match relation.kind {
            crate::model::RelationKind::Table => "database.table",
            crate::model::RelationKind::View => "database.view",
        },
        &relation.name,
    );
    if let Ok(action) = ActionSpec::operation(
        "browse",
        "Browse rows",
        OperationKind::Effect,
        DB_OPERATION_TYPE,
        &DbClientOperation::BrowseTable {
            connection_id: connection_id.to_string(),
            schema: schema.name.clone(),
            table: relation.name.clone(),
        },
    ) {
        resource
            .actions
            .push(job(action).retry_policy(RetryPolicy::Idempotent));
    }
    index_resource(
        rows,
        resources,
        resource.clone(),
        "browse",
        format!("{} {} {:?}", schema.name, relation.name, relation.kind),
        result,
    );
    for column in &relation.columns {
        let column_id = catalog_resource_id(
            "column",
            &[connection_id, &schema.name, &relation.name, &column.name],
        );
        let result =
            SearchResult::for_action(KIND, &column.name, ActionAddress::new(&column_id, "browse"))
                .with_subtitle(format!(
                    "{}.{} · {}",
                    schema.name, relation.name, column.decl_type
                ));
        let mut column_resource = Resource::new(&column_id, "database.column", &column.name);
        column_resource.actions.clone_from(&resource.actions);
        index_resource(
            rows,
            resources,
            column_resource,
            "browse",
            format!(
                "{} {} {} {}",
                schema.name, relation.name, column.name, column.decl_type
            ),
            result,
        );
    }
}

fn index_current_result(
    session: &Session,
    connection_id: &str,
    rows: &mut Vec<(String, String, SearchResult)>,
    resources: &mut HashMap<String, Resource>,
) {
    let result_id = catalog_resource_id(
        "query-result",
        &[connection_id, &session.query_gen.to_string()],
    );
    if let Some(result) = &session.query_result {
        let mut result_resource =
            Resource::new(&result_id, "database.query-result", result.message());
        if let Ok(action) = ActionSpec::operation(
            "show",
            "Show query result",
            OperationKind::View,
            DB_OPERATION_TYPE,
            &DbClientOperation::ShowResult {
                connection_id: connection_id.to_string(),
                result_generation: session.query_gen,
            },
        ) {
            result_resource.actions.push(action);
        }
        if result.is_select
            && let Ok(action) = ActionSpec::operation(
                "export_csv",
                "Export CSV",
                OperationKind::Effect,
                DB_OPERATION_TYPE,
                &DbClientOperation::ExportCsv {
                    connection_id: connection_id.to_string(),
                    result_generation: session.query_gen,
                },
            )
        {
            result_resource.actions.push(job(action));
        }
        index_resource(
            rows,
            resources,
            result_resource,
            "show",
            format!("query result {}", result.message()),
            SearchResult::for_action(
                KIND,
                "Current query result",
                ActionAddress::new(&result_id, "show"),
            )
            .with_subtitle(result.message()),
        );
    }
}

fn index_transactions(
    connection_id: &str,
    rows: &mut Vec<(String, String, SearchResult)>,
    resources: &mut HashMap<String, Resource>,
) {
    let transaction_id = catalog_resource_id("transaction", &[connection_id]);
    let mut transaction = Resource::new(&transaction_id, "database.transaction", "Transaction");
    for (action_id, label, sql, destructive) in [
        ("begin", "Begin transaction", "BEGIN", false),
        ("commit", "Commit transaction", "COMMIT", true),
        ("rollback", "Roll back transaction", "ROLLBACK", true),
    ] {
        if let Ok(action) = ActionSpec::operation(
            action_id,
            label,
            OperationKind::Effect,
            DB_OPERATION_TYPE,
            &DbClientOperation::RunQuery {
                connection_id: connection_id.to_string(),
                sql: sql.to_string(),
            },
        ) {
            transaction.actions.push(if destructive {
                job(action).destructive()
            } else {
                job(action)
            });
        }
    }
    index_resource(
        rows,
        resources,
        transaction,
        "begin",
        "transaction begin commit rollback",
        SearchResult::for_action(
            KIND,
            "Transaction",
            ActionAddress::new(&transaction_id, "begin"),
        )
        .with_subtitle("Active database"),
    );
}

fn index_query_history(
    session: &Session,
    connection: &crate::model::ConnectionProfile,
    connection_id: &str,
    rows: &mut Vec<(String, String, SearchResult)>,
    resources: &mut HashMap<String, Resource>,
) {
    let history_id = catalog_resource_id("query-history", &[connection_id]);
    resources.insert(
        history_id.clone(),
        Resource::new(&history_id, "database.query-history", "Local query history"),
    );
    for entry in session
        .query_history
        .iter()
        .filter(|entry| entry.connection_id == connection.id)
    {
        let entry_id = catalog_resource_id(
            "query-history-entry",
            &[connection_id, &entry.generation.to_string()],
        );
        let title = format!("Query {} · {}", entry.generation, entry.summary);
        let mut history_entry = Resource::new(&entry_id, "database.query-history-entry", &title);
        if let Ok(action) = ActionSpec::operation(
            "run",
            "Run historical query",
            OperationKind::Effect,
            DB_OPERATION_TYPE,
            &DbClientOperation::RunHistory {
                connection_id: connection_id.to_string(),
                generation: entry.generation,
            },
        ) {
            history_entry.actions.push(job(action).destructive());
        }
        index_resource(
            rows,
            resources,
            history_entry,
            "run",
            format!("query {} {}", entry.generation, entry.summary),
            SearchResult::for_action(KIND, &title, ActionAddress::new(&entry_id, "run"))
                .with_subtitle("Local only — never shared with collaborators"),
        );
    }
}

fn apply_catalog_snapshot(
    state: &Arc<RwLock<IndexState>>,
    revision: String,
    catalog: SearchCatalog,
    rows: Vec<(String, String, SearchResult)>,
    resources: HashMap<String, Resource>,
) -> Result<(), String> {
    // `replace_all` reuses the retained catalogue and advances its
    // generation exactly once, so every continuation from the previous
    // snapshot is rejected instead of accidentally becoming current when
    // a rebuilt catalog happens to have the same row count.
    if let Err(error) = catalog.replace_all(rows) {
        let error = format!("Database search index refresh failed: {error}");
        state
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_error = Some(error.clone());
        return Err(error);
    }
    let mut state = state
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    state.revision = Some(revision);
    state.catalog = Some(catalog);
    state.resources = resources;
    state.last_error = None;
    drop(state);
    Ok(())
}

impl DbSearchIndex {
    pub fn provider(&self) -> DbSearchProvider {
        DbSearchProvider {
            index: self.clone(),
        }
    }

    /// The bounded top of the live catalog: the open database and its
    /// schemas, the current result, the transaction controls and the query
    /// history. Tables, columns and history entries resolve by id.
    pub fn rows(&self) -> Vec<Resource> {
        const TOP: &[&str] = &[
            "database.database",
            "database.schema",
            "database.query-result",
            "database.transaction",
            "database.query-history",
        ];
        let mut rows: Vec<Resource> = self
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .resources
            .values()
            .filter(|resource| TOP.contains(&resource.kind.as_str()))
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        rows
    }

    pub fn resource(&self, id: &str) -> Option<Resource> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .resources
            .get(id)
            .cloned()
    }

    pub fn last_error(&self) -> Option<String> {
        self.state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_error
            .clone()
    }
}

fn index_resource(
    rows: &mut Vec<(String, String, SearchResult)>,
    resources: &mut HashMap<String, Resource>,
    resource: Resource,
    action_id: &str,
    search_text: impl Into<String>,
    result: SearchResult,
) {
    debug_assert!(resource.actions.iter().any(|action| action.id == action_id));
    let id = resource.id.as_str().to_string();
    rows.push((id.clone(), search_text.into(), result));
    resources.insert(id, resource);
}

pub struct DbSearchProvider {
    index: DbSearchIndex,
}

impl CatalogSearchProvider for DbSearchProvider {
    fn provider_name(&self) -> &str {
        KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        self.index
            .state
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .catalog
            .clone()
    }

    fn limited_work_budget(&self, limit: usize) -> usize {
        // A one-shot query inspects no more candidates than results it asked for.
        limit
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        self.index.resource(&address.resource).filter(|resource| {
            resource
                .actions
                .iter()
                .any(|action| action.id == address.action)
        })
    }
}

fn revision(session: &Session) -> String {
    let mut parts = String::new();
    push_revision_part(
        &mut parts,
        &format!(
            "active:{:?}:connected:{}",
            session.active_connection_id(),
            session.connected
        ),
    );
    for connection in &session.connections {
        push_revision_part(
            &mut parts,
            &format!(
                "connection:{}:{}:{}:{}",
                connection.id,
                connection.name,
                connection.subtitle(),
                connection.connected
            ),
        );
    }
    for schema in &session.schema.schemas {
        push_revision_part(&mut parts, &schema.name);
        for relation in &schema.tables {
            push_revision_part(
                &mut parts,
                &format!("relation:{}:{:?}", relation.name, relation.kind),
            );
            for column in &relation.columns {
                push_revision_part(
                    &mut parts,
                    &format!("column:{}:{}", column.name, column.decl_type),
                );
            }
        }
    }
    if let Some(result) = &session.query_result {
        push_revision_part(
            &mut parts,
            &format!("query-result:{}:{}", session.query_gen, result.message()),
        );
    }
    for entry in &session.query_history {
        push_revision_part(
            &mut parts,
            &format!(
                "history:{}:{}:{}",
                entry.connection_id, entry.generation, entry.summary
            ),
        );
    }
    parts
}

fn push_revision_part(parts: &mut String, value: &str) {
    // Names may contain punctuation; length-prefixing keeps distinct catalog
    // states distinct without relying on a separator convention.
    parts.push_str(&value.len().to_string());
    parts.push(':');
    parts.push_str(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};
    use ui::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider};

    fn assert_address_resolves(index: &DbSearchIndex, result: &SearchResult) {
        let address = result.action_address().expect("canonical action address");
        let resource = index
            .provider()
            .resolve_action_resource(&address)
            .expect("the provider resolves its exact canonical search address");
        assert!(
            resource
                .actions
                .iter()
                .any(|action| action.id == address.action),
            "search address points at an action published by the resolved resource"
        );
    }

    /// Relations and columns are found through bounded pages and exact addresses, limits are clamped, saved SQL is not indexed and resolution keeps every action.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn catalog_search_finds_relations_within_bounds() {
        // --- phase: live_relations_are_findable_through_bounded_catalog_pages ---
        {
            let session = Session::with_sample();
            let index = DbSearchIndex::default();
            index.sync(&session).unwrap();
            let provider = index.provider();
            let page = provider.search_page("albums", None, 20, 20);
            assert!(page.inspected <= 20);
            let hit = page
                .results
                .iter()
                .find(|result| result.title == "albums")
                .expect("relation search hit");
            assert_eq!(hit.title, "albums");
            assert_eq!(
                hit.action_address().expect("canonical address").action,
                "browse"
            );
            assert_address_resolves(&index, hit);
        }

        // --- phase: provider_clamps_untrusted_result_limits ---
        {
            let mut session = Session::with_sample();
            let template = session.schema.schemas[0].tables[0].clone();
            session.schema.schemas[0]
                .tables
                .extend((0..300).map(|number| crate::model::TableInfo {
                    name: format!("indexed_table_{number}"),
                    ..template.clone()
                }));
            let index = DbSearchIndex::default();
            index.sync(&session).unwrap();
            let provider = index.provider();

            assert_eq!(
                provider.search_limited("", usize::MAX).len(),
                DEFAULT_SEARCH_RESULT_LIMIT
            );
            assert_eq!(
                provider
                    .search_page("", None, usize::MAX, usize::MAX)
                    .results
                    .len(),
                DEFAULT_SEARCH_RESULT_LIMIT
            );
        }

        // --- phase: late_relation_is_discoverable_without_scanning_a_large_schema ---
        {
            let index = DbSearchIndex::default();
            let (_, hit) = assert_late_hit_reachable(
                |corpus| {
                    let mut session = Session::with_sample();
                    let mut template = session.schema.schemas[0].tables[0].clone();
                    // Only relations are under test; the fixture's columns would
                    // multiply the catalogue without adding late relations.
                    template.columns.clear();
                    session.schema.schemas[0].tables = corpus
                        .map(|row| {
                            let mut table = template.clone();
                            table.name = row.text;
                            table
                        })
                        .collect();
                    index.sync(&session).unwrap();
                    index.provider()
                },
                3_000,
            );
            assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
            assert_address_resolves(&index, &hit);
        }

        // --- phase: late_column_is_discoverable_through_its_own_exact_browse_address ---
        {
            let index = DbSearchIndex::default();
            let (_, hit) = assert_late_hit_reachable(
                |corpus| {
                    let mut session = Session::with_sample();
                    let template = session.schema.schemas[0].tables[0].columns[0].clone();
                    session.schema.schemas[0].tables[0].columns = corpus
                        .map(|row| {
                            let mut column = template.clone();
                            column.name = row.text;
                            column
                        })
                        .collect();
                    index.sync(&session).unwrap();
                    index.provider()
                },
                3_000,
            );
            assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
            assert_address_resolves(&index, &hit);
        }

        // --- phase: canonical_provider_resolution_retains_every_resource_action ---
        {
            let session = Session::with_sample();
            let index = DbSearchIndex::default();
            index.sync(&session).unwrap();
            let provider = index.provider();

            let cases = [
                (
                    "query result",
                    "Current query result",
                    &["show", "export_csv"] as &[_],
                ),
                (
                    "transaction",
                    "Transaction",
                    &["begin", "commit", "rollback"] as &[_],
                ),
            ];
            for (query, title, expected_actions) in cases {
                let hit = provider
                    .search_limited(query, 8)
                    .into_iter()
                    .find(|result| result.title == title)
                    .unwrap_or_else(|| panic!("canonical search hit for {title}"));
                let address = hit.action_address().expect("canonical address");
                let resource = provider
                    .resolve_action_resource(&address)
                    .expect("provider materializes the canonical resource");
                for action in expected_actions {
                    assert!(
                        resource
                            .actions
                            .iter()
                            .any(|published| published.id == *action),
                        "{title} keeps its {action} action after canonical resolution"
                    );
                }
            }
        }
    }

    /// Changed or renamed catalogs and a changed connection invalidate cursors and addresses; a rejected snapshot keeps the last good catalog.
    #[test]
    fn catalog_changes_invalidate_cursors_and_addresses() {
        // --- phase: changed_catalog_invalidates_old_search_cursors ---
        {
            let mut session = Session::with_sample();
            let index = DbSearchIndex::default();
            index.sync(&session).unwrap();
            let provider = index.provider();
            let first = provider.search_page("", None, 1, 1);
            let cursor = first.next.expect("large catalog has a continuation");
            let mut relation = session.schema.schemas[0].tables[0].clone();
            relation.name = "cursor-change".into();
            session.schema.schemas[0].tables.push(relation);
            index.sync(&session).unwrap();
            assert!(!provider.action_directory_cursor_is_current(&cursor));
        }

        // --- phase: renamed_catalog_fields_rebuild_the_retained_index ---
        {
            let mut session = Session::with_sample();
            let index = DbSearchIndex::default();
            index.sync(&session).unwrap();
            let provider = index.provider();
            let cursor = provider
                .search_page("", None, 1, 1)
                .next
                .expect("sample catalog is pageable");

            session.schema.schemas[0].tables[0].columns[0].name = "renamed_column".into();
            index.sync(&session).unwrap();
            assert!(!provider.action_directory_cursor_is_current(&cursor));
            let column = provider.search_limited("renamed_column", 4);
            assert_eq!(column.len(), 1);
            assert_address_resolves(&index, &column[0]);
        }

        // --- phase: changing_active_connection_invalidates_catalog_addresses ---
        {
            let mut session = Session::with_sample();
            let index = DbSearchIndex::default();
            index.sync(&session).unwrap();
            let provider = index.provider();
            let old_relation = provider.search_limited("albums", 4).remove(0);
            let cursor = provider
                .search_page("", None, 1, 1)
                .next
                .expect("sample catalog is pageable");

            session.active = 1;
            session.connected = false;
            index.sync(&session).unwrap();

            assert!(!provider.action_directory_cursor_is_current(&cursor));
            assert!(
                index
                    .resource(&old_relation.action_address().unwrap().resource)
                    .is_none(),
                "a relation cannot resolve against a disconnected, different active connection"
            );
        }

        // --- phase: rejected_snapshot_keeps_the_last_good_catalog_and_reports_the_failure ---
        {
            let session = Session::with_sample();
            let index = DbSearchIndex::default();
            index.sync(&session).unwrap();
            let before = index.provider().search_limited("albums", 8);
            assert!(!before.is_empty());

            let id = "duplicate-resource".to_string();
            let row = || {
                (
                    id.clone(),
                    "duplicate".to_string(),
                    SearchResult::for_action(KIND, "Duplicate", ActionAddress::new(&id, "browse")),
                )
            };
            let error = apply_catalog_snapshot(
                &index.state,
                "broken snapshot".to_string(),
                SearchCatalog::new(KIND),
                vec![row(), row()],
                HashMap::new(),
            )
            .expect_err("duplicate rows reject the snapshot");
            assert!(error.contains("Database search index refresh failed"));
            assert_eq!(index.last_error().as_deref(), Some(error.as_str()));
            assert_eq!(index.provider().search_limited("albums", 8), before);
        }
    }
}
