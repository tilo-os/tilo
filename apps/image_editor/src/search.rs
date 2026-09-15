use crate::model::Project;
use ui::search::{SearchProvider, SearchResult};
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
}
