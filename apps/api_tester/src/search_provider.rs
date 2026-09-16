//! Compact, secret-free API workspace search projection.

#[cfg(test)]
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};

use qos_actions::{ActionAddress, ActionSpec, ExecutionLocus, Resource};
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::{
    actions::{
        delete_collection_action, delete_environment_action, delete_request_action, encode,
        immediate, rename_collection_action, rename_environment_action, rename_request_action,
        save_request_action, send_request_operation, set_variable_action, variable_resource,
    },
    model::{Collection, Environment, Request},
};

pub const KIND: &str = "API workspace";

/// Retains only stable addresses and human-safe labels. Request bodies,
/// auth, and environment variable values are deliberately never copied here.
#[derive(Clone)]
pub struct ApiSearchProvider {
    catalog: SearchCatalog,
    indexed: BTreeSet<String>,
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
}

impl ApiSearchProvider {
    pub fn new() -> Self {
        Self {
            catalog: SearchCatalog::new(KIND),
            indexed: BTreeSet::new(),
            entries: BTreeMap::new(),
            #[cfg(test)]
            last_exact_inspected: Cell::new(0),
        }
    }
    pub fn refresh(&mut self, collections: &[Collection], environments: &[Environment]) {
        let mut next = BTreeSet::new();
        let mut entries = BTreeMap::new();
        for collection in collections {
            let cid = format!("api_tester.collection:{}", collection.id);
            self.upsert(
                &mut next,
                cid.clone(),
                "api_tester.collection",
                &collection.name,
                "rename",
            );
            entries.insert(
                cid,
                CompactEntry::Collection {
                    id: collection.id,
                    label: collection.name.clone(),
                },
            );
            for request in &collection.requests {
                let id = format!("api_tester.request:{}", request.id);
                let label = format!("{} {}", request.method.as_str(), request.name);
                self.upsert(&mut next, id.clone(), "api_tester.request", &label, "send");
                entries.insert(
                    id,
                    CompactEntry::Request {
                        collection_id: collection.id,
                        id: request.id,
                        label,
                    },
                );
            }
        }
        for environment in environments {
            let eid = format!("api_tester.environment:{}", environment.id);
            self.upsert(
                &mut next,
                eid.clone(),
                "api_tester.environment",
                &environment.name,
                "rename",
            );
            entries.insert(
                eid,
                CompactEntry::Environment {
                    id: environment.id,
                    label: environment.name.clone(),
                },
            );
            for variable in &environment.variables {
                let id = format!("api_tester.variable:{}:{}", environment.id, variable.id);
                let label = format!("Variable {}", variable.key);
                self.upsert(
                    &mut next,
                    id.clone(),
                    "api_tester.variable",
                    &label,
                    "set-value",
                );
                entries.insert(
                    id,
                    CompactEntry::Variable {
                        environment_id: environment.id,
                        id: variable.id.clone(),
                        key: variable.key.clone(),
                    },
                );
            }
        }
        for old in self.indexed.difference(&next) {
            self.catalog.remove(old);
        }
        self.indexed = next;
        self.entries = entries;
    }
    fn upsert(
        &self,
        ids: &mut BTreeSet<String>,
        id: String,
        kind: &str,
        label: &str,
        action: &str,
    ) {
        let _ = self.catalog.upsert(
            id.clone(),
            label,
            SearchResult::for_action(kind, label, ActionAddress::new(id.clone(), action)),
        );
        ids.insert(id);
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
        }
    }

    #[cfg(test)]
    pub fn last_exact_inspected(&self) -> usize {
        self.last_exact_inspected.get()
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
impl SearchProvider for ApiSearchProvider {
    fn name(&self) -> &str {
        KIND
    }
    fn action_directory_generation(&self) -> Option<u64> {
        Some(self.catalog.stats().generation)
    }
    fn action_directory_cursor_is_current(&self, cursor: &SearchCursor) -> bool {
        self.catalog.cursor_is_current(cursor)
    }
    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }
    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        self.catalog
            .search_limited(query, limit.min(DEFAULT_SEARCH_RESULT_LIMIT))
    }
    fn search_page(
        &self,
        q: &str,
        c: Option<&SearchCursor>,
        work: usize,
        limit: usize,
    ) -> SearchPage {
        self.catalog.query_page(q, c, work, limit)
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
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

    #[test]
    fn late_request_is_found_with_bounded_cursor_work_and_no_variable_values() {
        let mut collection = Collection::new("Load test");
        collection.requests = (0..100_001)
            .map(|i| Request::new(format!("Request {i}")))
            .collect();
        let mut environment = Environment::new("Secrets");
        environment
            .variables
            .push(crate::model::KeyValue::new("token", "do-not-index"));
        let mut provider = ApiSearchProvider::new();
        provider.refresh(&[collection], &[environment]);
        let hit = provider.search_page("Request 100000", None, 8, 1);
        assert!(hit.inspected <= 8 && hit.results.len() <= 1);
        assert_eq!(hit.results[0].title, "GET Request 100000");
        let address = hit.results[0].action_address().expect("canonical address");
        let resource = provider
            .resolve_action_resource(&address)
            .expect("the exact late request materializes on demand");
        assert_eq!(resource.id.0, address.resource);
        assert!(resource.actions.iter().any(|action| action.id == "send"));
        assert!(provider.search("do-not-index").is_empty());
        let first = provider.search_page("Request", None, 4, 1);
        let cursor = first.next.expect("broad result is paged");
        provider.refresh(&[], &[]);
        assert!(!provider.action_directory_cursor_is_current(&cursor));
    }

    #[test]
    fn same_key_variables_keep_distinct_resolvable_addresses_without_values() {
        let mut environment = Environment::new("Shared");
        environment.variables = vec![
            crate::model::KeyValue::new("token", "first-secret"),
            crate::model::KeyValue::new("token", "second-secret"),
        ];
        let mut provider = ApiSearchProvider::new();
        provider.refresh(&[], &[environment]);
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
}
