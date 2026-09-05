//! Bounded global-search projection for browser history and bookmarks.

use qos_actions::{ActionAddress, ActionSpec, OperationKind, Resource};
use ui::icons::Icon;
use ui::search::{SearchProvider, SearchResult, fuzzy_score};

const MAX_SCAN: usize = 5_000;

#[derive(Clone)]
struct Row {
    resource_id: String,
    title: String,
    url: String,
    kind: &'static str,
    icon: Icon,
    score: i32,
}

pub struct BrowserSearchProvider {
    rows: Vec<Row>,
}

impl BrowserSearchProvider {
    pub fn new(history: &crate::history::History, bookmarks: &crate::library::Bookmarks) -> Self {
        let mut rows = Vec::new();
        for entry in bookmarks.entries().iter().take(MAX_SCAN) {
            rows.push(Row {
                resource_id: id("bookmark", &entry.url),
                title: entry.title.clone(),
                url: entry.url.clone(),
                kind: "Bookmarks",
                icon: Icon::Star,
                score: 40,
            });
        }
        for entry in history
            .entries()
            .iter()
            .rev()
            .take(MAX_SCAN.saturating_sub(rows.len()))
        {
            rows.push(Row {
                resource_id: id("history", &entry.url),
                title: if entry.title.trim().is_empty() {
                    entry.url.clone()
                } else {
                    entry.title.clone()
                },
                url: entry.url.clone(),
                kind: "History",
                icon: Icon::History,
                score: i32::try_from(entry.visit_count.min(20)).unwrap_or(20),
            });
        }
        Self { rows }
    }

    fn row_resource(row: &Row) -> Resource {
        let mut resource = Resource::new(&row.resource_id, "browser.page-reference", &row.title);
        let payload = serde_json::json!({ "operation": "navigate", "url": row.url });
        if let Ok(spec) = ActionSpec::operation(
            "open",
            "Open page",
            OperationKind::Effect,
            "browser.operation",
            &payload,
        ) {
            resource.actions.push(spec);
        }
        resource
    }
}

fn id(kind: &str, url: &str) -> String {
    format!("browser.{kind}.{}", blake3::hash(url.as_bytes()).to_hex())
}

impl SearchProvider for BrowserSearchProvider {
    fn name(&self) -> &str {
        "Browser"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, 100)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        if limit == 0 {
            return Vec::new();
        }
        let query = query.trim();
        self.rows
            .iter()
            .filter_map(|row| {
                let match_score = if query.is_empty() {
                    0
                } else {
                    fuzzy_score(query, &row.title)
                        .into_iter()
                        .chain(fuzzy_score(query, &row.url))
                        .max()?
                };
                Some(
                    SearchResult::for_action(
                        row.kind,
                        &row.title,
                        ActionAddress::new(&row.resource_id, "open"),
                    )
                    .with_subtitle(&row.url)
                    .with_icon(row.icon)
                    .with_score(row.score.saturating_add(match_score)),
                )
            })
            .take(limit.min(100))
            .collect()
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        if address.action != "open" {
            return None;
        }
        self.rows
            .iter()
            .find(|row| row.resource_id == address.resource)
            .map(Self::row_resource)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materialization_and_results_are_bounded_and_canonical() {
        let mut history = crate::history::History::new();
        for index in 0..6_000 {
            history.record_at(
                &format!("https://{index}.example.test"),
                &format!("Page {index}"),
                index,
            );
        }
        let provider = BrowserSearchProvider::new(&history, &Default::default());
        assert_eq!(provider.rows.len(), MAX_SCAN);
        let hits = provider.search_limited("Page", 12);
        assert_eq!(hits.len(), 12);
        let address = hits[0].action_address().unwrap();
        assert!(provider.resolve_action_resource(&address).is_some());
    }
}
