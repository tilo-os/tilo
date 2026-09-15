use crate::model::Project;
use ui::search::{SearchCursor, SearchPage, SearchProvider, SearchResult};
#[derive(Clone)]
pub struct ProjectSearch(pub Project);
impl SearchProvider for ProjectSearch {
    fn name(&self) -> &str {
        "Image Editor"
    }
    fn search(&self, q: &str) -> Vec<SearchResult> {
        self.search_limited(q, usize::MAX)
    }
    fn search_limited(&self, q: &str, limit: usize) -> Vec<SearchResult> {
        let q = q.to_lowercase();
        self.0
            .graph
            .nodes
            .iter()
            .filter(|(_, n)| n.title.to_lowercase().contains(&q))
            .map(|(id, n)| {
                SearchResult::new(
                    "Image steps",
                    n.title.clone(),
                    format!("image-editor:node:{id}"),
                )
            })
            .take(limit)
            .collect()
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        // A project graph is an in-memory, single-page catalogue. Still cap
        // both inspected nodes and materialised hits so Ctrl+K never walks an
        // unexpectedly large imported graph in one UI turn.
        if cursor.is_some() || work_budget == 0 || result_limit == 0 {
            return SearchPage {
                results: Vec::new(),
                next: None,
                inspected: 0,
            };
        }
        let query = query.to_lowercase();
        let mut inspected = 0;
        let results = self
            .0
            .graph
            .nodes
            .iter()
            .take(work_budget)
            .inspect(|_| inspected += 1)
            .filter(|(_, node)| node.title.to_lowercase().contains(&query))
            .map(|(id, node)| {
                SearchResult::new(
                    "Image steps",
                    node.title.clone(),
                    format!("image-editor:node:{id}"),
                )
            })
            .take(result_limit)
            .collect();
        SearchPage {
            results,
            next: None,
            inspected,
        }
    }
}
