//! Compact, secret-free API workspace search projection.

#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use tilo_actions::{ActionAddress, ActionSpec, ExecutionLocus, Resource};
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchCatalogError, SearchResult};

use crate::{
    actions::{
        delete_collection_action, delete_environment_action, delete_request_action, encode,
        immediate, rename_collection_action, rename_environment_action, rename_request_action,
        save_current_request_action, save_request_action, send_request_operation,
        set_variable_action, variable_resource,
    },
    model::{Collection, Environment, HistoryEntry, Request},
};

pub const KIND: &str = "API workspace";

/// Retains only stable addresses and human-safe labels. Request bodies,
/// auth, and environment variable values are deliberately never copied here.
#[derive(Clone)]
pub struct ApiSearchProvider {
    catalog: SearchCatalog,
    entries: BTreeMap<String, CompactEntry>,
    #[cfg(test)]
    last_exact_inspected: Cell<usize>,
}

/// The resolver's retained projection.  It intentionally contains no request
/// body, auth, URL, or variable value: a search hit only needs the stable
/// action address and the minimal identifiers needed to reconstruct that one
/// action resource.
#[derive(Clone)]
enum CompactEntry {
    Collection {
        id: uuid::Uuid,
        label: String,
    },
    Request {
        collection_id: uuid::Uuid,
        id: uuid::Uuid,
        label: String,
    },
    Environment {
        id: uuid::Uuid,
        label: String,
    },
    Variable {
        environment_id: uuid::Uuid,
        id: String,
        key: String,
    },
    History {
        id: uuid::Uuid,
        label: String,
    },
}

impl ApiSearchProvider {
    pub fn new() -> Self {
        Self {
            catalog: SearchCatalog::new(KIND),
            entries: BTreeMap::new(),
            #[cfg(test)]
            last_exact_inspected: Cell::new(0),
        }
    }
    /// Atomically replace the compact public index at a workspace revision.
    /// On malformed/colliding records the last coherent catalogue and resolver
    /// entries stay live and the error is returned to the caller.
    pub fn refresh(
        &mut self,
        collections: &[Collection],
        environments: &[Environment],
        history: &[HistoryEntry],
    ) -> Result<(), SearchCatalogError> {
        let mut build = CatalogBuild::default();
        build.collections(collections)?;
        build.environments(environments)?;
        build.history(history)?;
        self.catalog.replace_all(build.rows)?;
        self.entries = build.entries;
        Ok(())
    }

    /// Materialize exactly one durable resource from the retained public
    /// lookup row. This does not read the document or traverse workspace
    /// collections; the only lookup is the collision-safe address map.
    pub fn resource(&self, id: &str) -> Option<Resource> {
        #[cfg(test)]
        self.last_exact_inspected.set(1);
        match self.entries.get(id)? {
            CompactEntry::Collection { id, label } => Some(collection_resource(*id, label)),
            CompactEntry::Request {
                collection_id,
                id,
                label,
            } => Some(request_resource(*collection_id, *id, label)),
            CompactEntry::Environment { id, label } => Some(environment_resource(*id, label)),
            CompactEntry::Variable {
                environment_id,
                id,
                key,
                ..
            } => Some(variable_resource(*environment_id, id, key)),
            CompactEntry::History { id, label } => Some(history_resource(*id, label)),
        }
    }

    #[cfg(test)]
    pub fn last_exact_inspected(&self) -> usize {
        self.last_exact_inspected.get()
    }
}

#[derive(Default)]
struct CatalogBuild {
    ids: BTreeSet<String>,
    entries: BTreeMap<String, CompactEntry>,
    rows: Vec<(String, String, SearchResult)>,
}

impl CatalogBuild {
    fn collections(&mut self, rows: &[Collection]) -> Result<(), SearchCatalogError> {
        for collection in rows {
            let id = format!("api_tester.collection:{}", collection.id);
            self.insert(
                id.clone(),
                "api_tester.collection",
                &collection.name,
                "rename",
                CompactEntry::Collection {
                    id: collection.id,
                    label: collection.name.clone(),
                },
            )?;
            for request in &collection.requests {
                let id = format!("api_tester.request:{}", request.id);
                let label = format!("{} {}", request.method.as_str(), request.name);
                self.insert(
                    id,
                    "api_tester.request",
                    &label,
                    "send",
                    CompactEntry::Request {
                        collection_id: collection.id,
                        id: request.id,
                        label: label.clone(),
                    },
                )?;
            }
        }
        Ok(())
    }
    fn environments(&mut self, rows: &[Environment]) -> Result<(), SearchCatalogError> {
        for environment in rows {
            let id = format!("api_tester.environment:{}", environment.id);
            self.insert(
                id,
                "api_tester.environment",
                &environment.name,
                "rename",
                CompactEntry::Environment {
                    id: environment.id,
                    label: environment.name.clone(),
                },
            )?;
            for variable in &environment.variables {
                let id = format!("api_tester.variable:{}:{}", environment.id, variable.id);
                let label = format!("Variable {}", variable.key);
                self.insert(
                    id,
                    "api_tester.variable",
                    &label,
                    "set-value",
                    CompactEntry::Variable {
                        environment_id: environment.id,
                        id: variable.id.clone(),
                        key: variable.key.clone(),
                    },
                )?;
            }
        }
        Ok(())
    }
    fn history(&mut self, rows: &[HistoryEntry]) -> Result<(), SearchCatalogError> {
        for entry in rows {
            let id = format!("api_tester.history:{}", entry.id);
            let label = history_label(entry);
            self.insert(
                id,
                "api_tester.history_entry",
                &label,
                "replay",
                CompactEntry::History {
                    id: entry.id,
                    label: label.clone(),
                },
            )?;
            if let Some(row) = self.rows.last_mut() {
                row.2 = row
                    .2
                    .clone()
                    .with_subtitle(format!("Status {}", entry.status));
            }
        }
        Ok(())
    }
    fn insert(
        &mut self,
        id: String,
        kind: &str,
        label: &str,
        action: &str,
        entry: CompactEntry,
    ) -> Result<(), SearchCatalogError> {
        if !self.ids.insert(id.clone()) || self.entries.insert(id.clone(), entry).is_some() {
            return Err(SearchCatalogError::DuplicateResourceId);
        }
        self.rows.push((
            id.clone(),
            label.to_string(),
            SearchResult::for_action(kind, label, ActionAddress::new(id, action)),
        ));
        Ok(())
    }
}

fn id_string(kind: &str, id: uuid::Uuid) -> String {
    format!("api_tester.{kind}:{id}")
}

fn collection_resource(id: uuid::Uuid, label: &str) -> Resource {
    Resource::new(id_string("collection", id), "api_tester.collection", label)
        .with_action(ActionSpec::prompted(
            "rename",
            "Rename collection",
            "Collection name",
            "new name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&rename_collection_action(id, name.trim())),
        ))
        .with_action(immediate(
            "delete",
            "Delete collection",
            &delete_collection_action(id),
        ))
        .with_action(ActionSpec::prompted(
            "add-request",
            "Add request",
            "Request name",
            "e.g. List users",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let mut request = Request::new(name.trim());
                request.id = uuid::Uuid::new_v4();
                encode(&save_request_action(id, request))
            },
        ))
        .with_action(immediate(
            "save-active",
            "Save current request here",
            &save_current_request_action(id, uuid::Uuid::new_v4()),
        ))
}

fn request_resource(collection_id: uuid::Uuid, id: uuid::Uuid, label: &str) -> Resource {
    let mut resource = Resource::new(id_string("request", id), "api_tester.request", label)
        .with_action(ActionSpec::prompted(
            "rename",
            "Rename request",
            "Request name",
            "new name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&rename_request_action(collection_id, id, name.trim())),
        ))
        .with_action(immediate(
            "delete",
            "Delete request",
            &delete_request_action(collection_id, id),
        ));
    if let Ok(send) = send_request_operation(collection_id, id) {
        resource = resource.with_action(send);
    }
    resource
}

fn environment_resource(id: uuid::Uuid, label: &str) -> Resource {
    Resource::new(
        id_string("environment", id),
        "api_tester.environment",
        label,
    )
    .with_action(immediate(
        "activate",
        format!("Use environment “{label}”"),
        &crate::actions::set_active_environment_action(Some(id)),
    ))
    .with_action(ActionSpec::prompted(
        "rename",
        "Rename environment",
        "Environment name",
        "new name…",
        ExecutionLocus::ClientLocal,
        move |name: &str| encode(&rename_environment_action(id, name.trim())),
    ))
    .with_action(immediate(
        "delete",
        "Delete environment",
        &delete_environment_action(id),
    ))
    .with_action(ActionSpec::prompted(
        "set-variable",
        "Set variable",
        "key=value",
        "e.g. base_url=https://api.example.com",
        ExecutionLocus::ClientLocal,
        move |arg: &str| {
            let (key, value) = arg.split_once('=').unwrap_or((arg, ""));
            encode(&set_variable_action(
                id,
                uuid::Uuid::new_v4().to_string(),
                key.trim(),
                value.trim(),
            ))
        },
    ))
}

fn history_resource(id: uuid::Uuid, label: &str) -> Resource {
    let mut resource = Resource::new(
        format!("api_tester.history:{id}"),
        "api_tester.history_entry",
        label,
    );
    if let Ok(action) = crate::actions::replay_history_operation(id) {
        resource = resource.with_action(action);
    }
    resource
}

/// Preserve enough route context to find a history entry without indexing URL
/// userinfo, query values, or fragments. A malformed URL gets a truthful but
/// non-sensitive fallback instead of leaking its raw text into the catalogue.
fn history_label(entry: &HistoryEntry) -> String {
    let Some((scheme, remainder)) = entry.url.split_once("://") else {
        return format!("{} request history", entry.method.as_str());
    };
    let safe = remainder.split(['?', '#']).next().unwrap_or_default();
    let Some((authority, path)) = safe.split_once('/') else {
        let authority = safe.rsplit('@').next().unwrap_or_default();
        return format!("{} {scheme}://{authority}", entry.method.as_str());
    };
    let authority = authority.rsplit('@').next().unwrap_or_default();
    format!("{} {scheme}://{authority}/{path}", entry.method.as_str())
}
impl CatalogSearchProvider for ApiSearchProvider {
    fn provider_name(&self) -> &str {
        KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn resolve_action_address(&self, address: &ActionAddress) -> Option<Resource> {
        let resource = self.resource(address.resource.as_str())?;
        resource
            .actions
            .iter()
            .any(|action| action.id == address.action)
            .then_some(resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Request;
    use ui::search::SearchProvider;
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};

    /// The catalog indexes requests and variables by stable address, never by
    /// variable value, and a refresh replaces it atomically.
    #[test]
    fn catalog_refresh_indexes_records_by_address_without_values() {
        // Case "late request": found with bounded cursor work and no variable
        // values; a later refresh invalidates the old cursor.
        let (mut provider, hit) = assert_late_hit_reachable(
            |corpus| {
                let mut collection = Collection::new("Load test");
                collection.requests = corpus
                    .map(|row| {
                        let mut request = Request::new(row.text);
                        request.id = uuid::Uuid::from_u128(row.index as u128 + 1);
                        request
                    })
                    .collect();
                let mut environment = Environment::new("Secrets");
                environment
                    .variables
                    .push(crate::model::KeyValue::new("token", "do-not-index"));
                let mut provider = ApiSearchProvider::new();
                provider
                    .refresh(&[collection], &[environment], &[])
                    .expect("valid workspace catalog");
                provider
            },
            3_000,
        );
        assert_eq!(
            hit.title,
            format!("GET ordinary record 2999 {LATE_HIT_NEEDLE}")
        );
        let address = hit.action_address().expect("canonical address");
        let resource = provider
            .resolve_action_resource(&address)
            .expect("the exact late request materializes on demand");
        assert_eq!(resource.id.0, address.resource);
        assert!(resource.actions.iter().any(|action| action.id == "send"));
        assert!(provider.search("do-not-index").is_empty());
        let first = provider.search_page(&ui::search::SearchQuery::new("ordinary"), None, 4, 1);
        let cursor = first.next.expect("broad result is paged");
        provider.refresh(&[], &[], &[]).expect("empty catalog");
        assert!(!provider.action_directory_cursor_is_current(&cursor));

        same_key_variables_keep_distinct_resolvable_addresses_without_values();
        failed_refresh_keeps_the_previous_catalog_and_exact_resolver_coherent();
    }

    /// Case "same-key variables" of
    /// `catalog_refresh_indexes_records_by_address_without_values`.
    fn same_key_variables_keep_distinct_resolvable_addresses_without_values() {
        let mut environment = Environment::new("Shared");
        environment.variables = vec![
            crate::model::KeyValue::new("token", "first-secret"),
            crate::model::KeyValue::new("token", "second-secret"),
        ];
        let mut provider = ApiSearchProvider::new();
        provider
            .refresh(&[], &[environment], &[])
            .expect("valid variable catalog");
        let hits = provider.search("Variable token");
        assert_eq!(hits.len(), 2);
        let addresses: BTreeSet<_> = hits
            .iter()
            .map(|hit| hit.action_address().expect("address").resource)
            .collect();
        assert_eq!(
            addresses.len(),
            2,
            "ids, not display keys, identify variables"
        );
        for hit in hits {
            let address = hit.action_address().expect("address");
            let resource = provider
                .resolve_action_resource(&address)
                .expect("exact variable action");
            let actions: BTreeSet<_> = resource
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect();
            assert!(actions.is_superset(&BTreeSet::from(["rename", "set-value", "delete"])));
            assert!(!format!("{:?}", resource.label).contains("secret"));
        }
        assert!(provider.search("first-secret").is_empty());
        assert!(provider.search("second-secret").is_empty());
    }

    /// Case "failed refresh" of
    /// `catalog_refresh_indexes_records_by_address_without_values`.
    fn failed_refresh_keeps_the_previous_catalog_and_exact_resolver_coherent() {
        let collection = Collection::new("Stable collection");
        let id = format!("api_tester.collection:{}", collection.id);
        let mut provider = ApiSearchProvider::new();
        provider
            .refresh(std::slice::from_ref(&collection), &[], &[])
            .expect("initial catalog");
        let before = provider.catalog.stats().generation;

        let mut duplicate = collection.clone();
        duplicate.name = "Conflicting duplicate".to_string();
        assert_eq!(
            provider.refresh(&[collection, duplicate], &[], &[]),
            Err(SearchCatalogError::DuplicateResourceId)
        );
        assert_eq!(provider.catalog.stats().generation, before);
        assert_eq!(provider.search("Stable collection").len(), 1);
        assert!(provider.resource(&id).is_some());
        assert!(provider.search("Conflicting duplicate").is_empty());
    }

    #[test]
    fn history_is_searchable_redacted_and_materializes_its_replay_operation() {
        // Case "replay": a history hit resolves to its replay operation.
        let entry = HistoryEntry {
            id: uuid::Uuid::new_v4(),
            method: crate::model::HttpMethod::Get,
            url: "https://api.example.test/history".to_string(),
            status: 200,
            duration_ms: 12,
            when: "now".to_string(),
        };
        let mut provider = ApiSearchProvider::new();
        provider
            .refresh(&[], &[], std::slice::from_ref(&entry))
            .expect("history catalog");
        let hit = provider
            .search("example.test/history")
            .pop()
            .expect("history hit");
        let address = hit.action_address().expect("canonical replay address");
        let resource = provider
            .resolve_action_resource(&address)
            .expect("replay resource");
        assert!(resource.actions.iter().any(|action| action.id == "replay"));

        // Case "redaction": the catalog drops userinfo, query and fragment
        // secrets while keeping the route searchable.
        let entry = HistoryEntry {
            id: uuid::Uuid::new_v4(),
            method: crate::model::HttpMethod::Get,
            url: "https://alice:password@example.test/audit?token=query-secret#fragment-secret"
                .to_string(),
            status: 200,
            duration_ms: 12,
            when: "now".to_string(),
        };
        let mut provider = ApiSearchProvider::new();
        provider
            .refresh(&[], &[], std::slice::from_ref(&entry))
            .expect("history catalog");
        assert_eq!(provider.search("example.test/audit").len(), 1);
        for secret in ["alice", "password", "query-secret", "fragment-secret"] {
            assert!(
                provider.search(secret).is_empty(),
                "{secret} leaked into history search"
            );
        }
    }
}
