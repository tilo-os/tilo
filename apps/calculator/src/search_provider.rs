//! Evaluated expressions as a bounded, canonical system-search source.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, RwLock};

use tilo_actions::{ActionAddress, Resource};
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::model::HistoryEntry;

pub const HISTORY_KIND: &str = "Calculations";

#[derive(Clone)]
pub struct CalculatorSearchProvider {
    catalog: SearchCatalog,
    resources: Arc<RwLock<BTreeMap<String, Resource>>>,
    indexed_ids: Arc<RwLock<BTreeSet<String>>>,
}

impl CalculatorSearchProvider {
    pub fn new(history: &[HistoryEntry], stable_ids: &[String]) -> Self {
        let provider = Self {
            catalog: SearchCatalog::new(HISTORY_KIND),
            resources: Arc::new(RwLock::new(BTreeMap::new())),
            indexed_ids: Arc::new(RwLock::new(BTreeSet::new())),
        };
        provider.refresh(history, stable_ids);
        provider
    }

    /// Reconcile the complete loaded history without replacing the retained catalogue handle.
    pub fn refresh(&self, history: &[HistoryEntry], stable_ids: &[String]) {
        let mut resources = BTreeMap::new();
        for (index, entry) in history.iter().enumerate() {
            let Some(stable_id) = stable_ids.get(index) else {
                continue;
            };
            let id = format!("calculator.history_entry:{stable_id}");
            let title = if entry.expr.trim().is_empty() {
                entry.result.trim().to_owned()
            } else {
                entry.expr.trim().to_owned()
            };
            let resource =
                crate::actions::recall_expression_resource(&id, &title, entry.expr.trim());
            let result = SearchResult::for_action(
                HISTORY_KIND,
                &title,
                crate::actions::recall_expression_address(&id),
            )
            .with_subtitle(format!("Result · {}", entry.result.trim()))
            .with_icon(Icon::History);
            let _ = self
                .catalog
                .upsert(&id, format!("{}\0{}", entry.expr, entry.result), result);
            resources.insert(id, resource);
        }
        let new_ids: BTreeSet<_> = resources.keys().cloned().collect();
        let mut indexed = self
            .indexed_ids
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for stale in indexed.difference(&new_ids) {
            self.catalog.remove(stale);
        }
        *indexed = new_ids;
        *self
            .resources
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = resources;
    }
}

impl SearchProvider for CalculatorSearchProvider {
    fn name(&self) -> &str {
        HISTORY_KIND
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
        self.catalog.search_limited(query, limit)
    }
    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.catalog
            .query_page(query, cursor, work_budget, result_limit)
    }
    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        self.resources
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&address.resource)
            .filter(|resource| {
                resource
                    .actions
                    .iter()
                    .any(|action| action.id == address.action)
            })
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(expr: &str, result: &str) -> HistoryEntry {
        HistoryEntry {
            expr: expr.into(),
            result: result.into(),
        }
    }

    #[test]
    fn empty_history_has_no_hits() {
        assert!(
            CalculatorSearchProvider::new(&[], &[])
                .search("")
                .is_empty()
        );
    }

    #[test]
    fn pages_are_bounded_and_activation_resolves_to_recall() {
        let history: Vec<_> = (0..9)
            .map(|n| entry(&format!("{n}+1"), &format!("{}", n + 1)))
            .collect();
        let ids: Vec<_> = (0..history.len()).map(|n| format!("id-{n}")).collect();
        let provider = CalculatorSearchProvider::new(&history, &ids);
        let first = provider.search_page("", None, 2, 2);
        assert_eq!(first.results.len(), 2);
        assert!(first.inspected <= 2);
        let address = first.results[0].action_address().expect("canonical result");
        let resource = provider
            .resolve_action_resource(&address)
            .expect("current resource");
        assert_eq!(resource.kind, "calculator.history_entry");
        assert_eq!(address.action, "recall");
        let encoded = tilo_actions::door::encode(&[resource], &address, None)
            .expect("search activation uses the exported operation");
        assert!(matches!(
            crate::actions::decode_operation(&encoded.type_id, &encoded.payload)
                .expect("valid operation"),
            Some(crate::actions::CalculatorOperation::RecallExpression { .. })
        ));
        let second = provider.search_page("", first.next.as_ref(), 2, 2);
        assert_eq!(second.results.len(), 2);
        assert!(second.inspected <= 2);
    }

    #[test]
    fn refresh_updates_retained_clones_and_invalidates_old_cursor() {
        let provider = CalculatorSearchProvider::new(
            &[entry("2+2", "4"), entry("3+3", "6")],
            &["old-a".into(), "old-b".into()],
        );
        let clone = provider.clone();
        let cursor = provider.search_page("", None, 1, 1).next;
        provider.refresh(&[entry("sqrt(9)", "3")], &["new".into()]);
        assert!(clone.search("2+2").is_empty());
        assert_eq!(clone.search("sqrt").len(), 1);
        assert!(!clone.action_directory_cursor_is_current(
            cursor.as_ref().expect("the first bounded page continues")
        ));
    }
}
