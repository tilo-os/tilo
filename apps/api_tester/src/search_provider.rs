//! Compact, secret-free API workspace search projection.

use std::collections::BTreeSet;

use qos_actions::ActionAddress;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::model::{Collection, Environment};

pub const KIND: &str = "API workspace";

/// Retains only stable addresses and human-safe labels. Request bodies,
/// auth, and environment variable values are deliberately never copied here.
#[derive(Clone)]
pub struct ApiSearchProvider {
    catalog: SearchCatalog,
    indexed: BTreeSet<String>,
}

impl ApiSearchProvider {
    pub fn new() -> Self {
        Self {
            catalog: SearchCatalog::new(KIND),
            indexed: BTreeSet::new(),
        }
    }
    pub fn refresh(&mut self, collections: &[Collection], environments: &[Environment]) {
        let mut next = BTreeSet::new();
        for collection in collections {
            let cid = format!("api_tester.collection:{}", collection.id);
            self.upsert(
                &mut next,
                cid,
                "api_tester.collection",
                &collection.name,
                "rename",
            );
            for request in &collection.requests {
                let id = format!("api_tester.request:{}", request.id);
                self.upsert(
                    &mut next,
                    id,
                    "api_tester.request",
                    &format!("{} {}", request.method.as_str(), request.name),
                    "send",
                );
            }
        }
        for environment in environments {
            let eid = format!("api_tester.environment:{}", environment.id);
            self.upsert(
                &mut next,
                eid,
                "api_tester.environment",
                &environment.name,
                "rename",
            );
            for variable in &environment.variables {
                let id = format!("api_tester.variable:{}:{}", environment.id, variable.id);
                self.upsert(
                    &mut next,
                    id,
                    "api_tester.variable",
                    &format!("Variable {}", variable.key),
                    "set-value",
                );
            }
        }
        for old in self.indexed.difference(&next) {
            self.catalog.remove(old);
        }
        self.indexed = next;
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
        assert!(provider.search("do-not-index").is_empty());
        let first = provider.search_page("Request", None, 4, 1);
        let cursor = first.next.expect("broad result is paged");
        provider.refresh(&[], &[]);
        assert!(!provider.action_directory_cursor_is_current(&cursor));
    }
}
