//! The open connection's catalogue: its live rows and their search entries.
//!
//! Catalog discovery happens when the live schema or shared document changes,
//! never on each palette keystroke. Every relation, column, result,
//! transaction and history entry is a search entry the surface contributes
//! to the app's one search source, which pages through very large schemas.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ui::tilo_actions::Resource;
use ui::tilo_resource::{Effect, SearchEntry};
use uuid::Uuid;

use crate::app::catalog_resource_id;
use crate::resource::DbEffect;
use crate::session::Session;
use ui::lock::RwLockRecover as _;

const KIND: &str = "Database";

#[derive(Default)]
struct IndexState {
    revision: Option<String>,
    entries: Vec<SearchEntry>,
    resources: HashMap<String, Resource>,
    /// The most recent rejected rebuild. Keep the last known-good snapshot
    /// available, but never make a failed index refresh invisible to the app.
    last_error: Option<String>,
}

/// A cheap-to-clone handle to the open connection's catalogue.
#[derive(Clone, Default)]
pub struct DbSearchIndex {
    state: Arc<RwLock<IndexState>>,
}

impl DbSearchIndex {
    /// Refresh the catalogue from the live session.
    ///
    /// # Errors
    ///
    /// Returns an error when two rows of the snapshot name one resource. The
    /// previous valid snapshot remains available.
    pub fn sync(&self, session: &Session) -> Result<(), String> {
        let revision = revision(session);
        let state = self.state.read_recover();
        if state.revision.as_deref() == Some(&revision) {
            return Ok(());
        }
        drop(state);

        let mut rows = Vec::new();
        let mut resources = HashMap::new();
        index_active_catalog(session, &mut rows, &mut resources);
        apply_catalog_snapshot(&self.state, revision, rows, resources)
    }
}

fn index_active_catalog(
    session: &Session,
    rows: &mut Vec<(String, SearchEntry)>,
    resources: &mut HashMap<String, Resource>,
) {
    let Some(connection) = session.active_connection().filter(|_| session.connected) else {
        return;
    };
    index_database_and_schema(session, connection, rows, resources);
    index_current_result(session, connection.id, rows, resources);
    index_transactions(connection.id, rows, resources);
    index_query_history(session, connection, rows, resources);
}

fn index_database_and_schema(
    session: &Session,
    connection: &crate::model::ConnectionProfile,
    rows: &mut Vec<(String, SearchEntry)>,
    resources: &mut HashMap<String, Resource>,
) {
    let connection_id = &connection.id.to_string();
    let database_name = if connection.database.is_empty() {
        "Connected database".to_string()
    } else {
        connection.database.clone()
    };
    let database_id = catalog_resource_id("database", &[connection_id, &database_name]);
    let focus = DbEffect::FocusDatabase {
        connection: connection.id,
    };
    let database =
        Resource::new(&database_id, "database.database", &database_name).with_action(focus.bound());
    index_resource(
        rows,
        resources,
        database,
        focus.id(),
        database_name.clone(),
        hit(&database_name, &database_id).subtitled("Active database"),
    );
    for schema in &session.schema.schemas {
        let schema_id = catalog_resource_id("schema", &[connection_id, &schema.name]);
        let focus = DbEffect::FocusSchema {
            connection: connection.id,
            schema: schema.name.clone(),
        };
        let schema_resource =
            Resource::new(&schema_id, "database.schema", &schema.name).with_action(focus.bound());
        index_resource(
            rows,
            resources,
            schema_resource,
            focus.id(),
            schema.name.clone(),
            hit(&schema.name, &schema_id).subtitled(format!("Schema in {database_name}")),
        );
        for relation in &schema.tables {
            index_relation(connection.id, schema, relation, rows, resources);
        }
    }
}

fn index_relation(
    connection: Uuid,
    schema: &crate::model::SchemaInfo,
    relation: &crate::model::TableInfo,
    rows: &mut Vec<(String, SearchEntry)>,
    resources: &mut HashMap<String, Resource>,
) {
    let connection_id = &connection.to_string();
    let relation_id =
        catalog_resource_id("relation", &[connection_id, &schema.name, &relation.name]);
    let result =
        hit(&relation.name, &relation_id).subtitled(format!("{}.{}", schema.name, relation.name));
    let resource = Resource::new(
        &relation_id,
        match relation.kind {
            crate::model::RelationKind::Table => "database.table",
            crate::model::RelationKind::View => "database.view",
        },
        &relation.name,
    )
    .with_action(
        DbEffect::Browse {
            connection,
            schema: schema.name.clone(),
            table: relation.name.clone(),
        }
        .bound(),
    );
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
        let result = hit(&column.name, &column_id).subtitled(format!(
            "{} in {}.{}",
            column.decl_type, schema.name, relation.name
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
    connection: Uuid,
    rows: &mut Vec<(String, SearchEntry)>,
    resources: &mut HashMap<String, Resource>,
) {
    let result_id = catalog_resource_id(
        "query-result",
        &[&connection.to_string(), &session.query_gen.to_string()],
    );
    if let Some(result) = &session.query_result {
        let generation = session.query_gen;
        let show = DbEffect::ShowResult {
            connection,
            generation,
        };
        let mut result_resource =
            Resource::new(&result_id, "database.query-result", result.message())
                .with_action(show.bound());
        if result.is_select {
            result_resource = result_resource.with_action(
                DbEffect::ExportCsv {
                    connection,
                    generation,
                }
                .bound(),
            );
        }
        index_resource(
            rows,
            resources,
            result_resource,
            show.id(),
            format!("query result {}", result.message()),
            hit("Current query result", &result_id).subtitled(result.message()),
        );
    }
}

fn index_transactions(
    connection: Uuid,
    rows: &mut Vec<(String, SearchEntry)>,
    resources: &mut HashMap<String, Resource>,
) {
    let transaction_id = catalog_resource_id("transaction", &[&connection.to_string()]);
    let begin = DbEffect::Begin { connection };
    let transaction = Resource::new(&transaction_id, "database.transaction", "Transaction")
        .with_action(begin.bound())
        .with_action(DbEffect::Commit { connection }.bound())
        .with_action(DbEffect::Rollback { connection }.bound());
    index_resource(
        rows,
        resources,
        transaction,
        begin.id(),
        "transaction begin commit rollback",
        hit("Transaction", &transaction_id).subtitled("Active database"),
    );
}

fn index_query_history(
    session: &Session,
    connection: &crate::model::ConnectionProfile,
    rows: &mut Vec<(String, SearchEntry)>,
    resources: &mut HashMap<String, Resource>,
) {
    let connection_id = &connection.id.to_string();
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
        let title = format!("Query {}: {}", entry.generation, entry.summary);
        let rerun = DbEffect::Rerun {
            connection: connection.id,
            generation: entry.generation,
        };
        let history_entry = Resource::new(&entry_id, "database.query-history-entry", &title)
            .with_action(rerun.bound());
        index_resource(
            rows,
            resources,
            history_entry,
            rerun.id(),
            format!("query {} {}", entry.generation, entry.summary),
            hit(&title, &entry_id).subtitled("Local only — never shared with collaborators"),
        );
    }
}

fn apply_catalog_snapshot(
    state: &Arc<RwLock<IndexState>>,
    revision: String,
    rows: Vec<(String, SearchEntry)>,
    resources: HashMap<String, Resource>,
) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    if let Some((duplicate, _)) = rows.iter().find(|(id, _)| !seen.insert(id.as_str())) {
        let error = format!("Database search index refresh failed: `{duplicate}` is listed twice");
        state.write_recover().last_error = Some(error.clone());
        return Err(error);
    }
    let mut state = state.write_recover();
    state.revision = Some(revision);
    state.entries = rows.into_iter().map(|(_, entry)| entry).collect();
    state.resources = resources;
    state.last_error = None;
    drop(state);
    Ok(())
}

impl DbSearchIndex {
    /// What opening the row anchored at `anchor` runs: the effect its hit
    /// was found for (browse a relation, focus the database, show the
    /// result, rerun a query). `None` once the row is gone.
    pub fn opened(&self, anchor: &str) -> Option<DbEffect> {
        let resource = self.resource(anchor)?;
        let encoded = resource
            .actions
            .first()?
            .build_json(&serde_json::Map::new())
            .ok()?;
        DbEffect::from_payload(&encoded.payload).ok()
    }

    /// Every row of the catalogue as a search entry.
    pub fn entries(&self) -> Vec<SearchEntry> {
        self.state.read_recover().entries.clone()
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
            .read_recover()
            .resources
            .values()
            .filter(|resource| TOP.contains(&resource.kind.as_str()))
            .cloned()
            .collect();
        rows.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        rows
    }

    pub fn resource(&self, id: &str) -> Option<Resource> {
        self.state.read_recover().resources.get(id).cloned()
    }

    pub fn last_error(&self) -> Option<String> {
        self.state.read_recover().last_error.clone()
    }
}

/// A catalogue hit titled `title`, anchored at the row `id`.
fn hit(title: impl Into<String>, id: &str) -> SearchEntry {
    SearchEntry::new(title, String::new())
        .anchored(id)
        .grouped(KIND)
}

fn index_resource(
    rows: &mut Vec<(String, SearchEntry)>,
    resources: &mut HashMap<String, Resource>,
    resource: Resource,
    action_id: &str,
    search_text: impl Into<String>,
    mut entry: SearchEntry,
) {
    let mut resource = resource;
    // What opening the row runs comes first ([`DbSearchIndex::opened`]).
    let opens = resource
        .actions
        .iter()
        .position(|action| action.id == action_id);
    debug_assert!(opens.is_some());
    if let Some(at) = opens {
        let action = resource.actions.remove(at);
        resource.actions.insert(0, action);
    }
    let id = resource.id.as_str().to_string();
    entry.text = search_text.into();
    rows.push((id.clone(), entry));
    resources.insert(id, resource);
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
    use ui::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult};

    /// The app's search source over the index's entries, as the framework
    /// builds it.
    fn source(index: &DbSearchIndex) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<crate::record::Workspace>(index.entries())
    }

    fn assert_address_resolves(index: &DbSearchIndex, result: &SearchResult) {
        let resource = index
            .resource(&result.action_id)
            .expect("the live row of the hit's anchor resolves");
        let opened = index.opened(&result.action_id).expect("the hit opens");
        assert_eq!(
            resource.actions[0].id,
            opened.id(),
            "the hit opens with an action its row publishes"
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
            let provider = source(&index);
            let page = provider.search_page(&ui::search::SearchQuery::new("albums"), None, 20, 20);
            assert!(page.inspected <= 20);
            let hit = page
                .results
                .iter()
                .find(|result| result.title == "albums")
                .expect("relation search hit");
            assert_eq!(hit.title, "albums");
            assert_eq!(
                index.opened(&hit.action_id).map(|effect| effect.id()),
                Some("browse")
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
            let provider = source(&index);

            assert_eq!(
                provider.search_limited("", usize::MAX).len(),
                DEFAULT_SEARCH_RESULT_LIMIT
            );
            assert_eq!(
                provider
                    .search_page(
                        &ui::search::SearchQuery::new(""),
                        None,
                        usize::MAX,
                        usize::MAX
                    )
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
                    source(&index)
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
                    source(&index)
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
            let provider = source(&index);

            let cases = [
                (
                    "query result",
                    "Current query result",
                    &["show-result", "export-csv"] as &[_],
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
                let resource = index
                    .resource(&hit.action_id)
                    .expect("the live row materializes the canonical resource");
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
        // --- phase: renamed_catalog_fields_are_found_by_their_new_names ---
        {
            let mut session = Session::with_sample();
            let index = DbSearchIndex::default();
            index.sync(&session).unwrap();
            let before = index.entries();

            session.schema.schemas[0].tables[0].columns[0].name = "renamed_column".into();
            index.sync(&session).unwrap();
            assert_ne!(index.entries(), before, "a renamed column is new entries");
            let column = source(&index).search_limited("renamed_column", 4);
            assert_eq!(column.len(), 1);
            assert_address_resolves(&index, &column[0]);
        }

        // --- phase: changing_active_connection_invalidates_catalog_addresses ---
        {
            let mut session = Session::with_sample();
            let index = DbSearchIndex::default();
            index.sync(&session).unwrap();
            let provider = source(&index);
            let old_relation = provider.search_limited("albums", 4).remove(0);

            session.active = 1;
            session.connected = false;
            index.sync(&session).unwrap();

            assert!(source(&index).search_limited("albums", 4).is_empty());
            assert!(
                index.resource(&old_relation.action_id).is_none(),
                "a relation cannot resolve against a disconnected, different active connection"
            );
        }

        // --- phase: rejected_snapshot_keeps_the_last_good_catalog_and_reports_the_failure ---
        {
            let session = Session::with_sample();
            let index = DbSearchIndex::default();
            index.sync(&session).unwrap();
            let before = source(&index).search_limited("albums", 8);
            assert!(!before.is_empty());

            let id = "duplicate-resource".to_string();
            let row = || (id.clone(), hit("Duplicate", &id));
            let error = apply_catalog_snapshot(
                &index.state,
                "broken snapshot".to_string(),
                vec![row(), row()],
                HashMap::new(),
            )
            .expect_err("duplicate rows reject the snapshot");
            assert!(error.contains("Database search index refresh failed"));
            assert_eq!(index.last_error().as_deref(), Some(error.as_str()));
            assert_eq!(source(&index).search_limited("albums", 8), before);
        }
    }
}
