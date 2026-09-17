//! Complete, position-aware search over the open qdoc document.
//!
//! The renderer deliberately virtualises large documents; search must not. This
//! provider retains a complete, incrementally replaced text catalogue and maps
//! every result back to the existing `NavigateTo` resource operation so palette,
//! accessibility and automation all use the same focus path.

use std::sync::{Arc, RwLock};

use tilo_actions::{ActionAddress, ActionSpec, OperationKind, Resource};
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCursor, SearchPage, SearchProvider,
    SearchResult,
};

use crate::actions::{WORD_PROCESSOR_OPERATION_TYPE, WordProcessorOperation};

pub const DOCUMENT_TEXT_KIND: &str = "Document text";
const RESOURCE_PREFIX: &str = "word-processor.text-hit:";
const OPEN_ACTION: &str = "go-to";

#[derive(Clone, Debug, PartialEq, Eq)]
struct SearchBlock {
    id: String,
    text: String,
}

#[derive(Clone)]
pub struct WordProcessorSearchProvider {
    catalog: SearchCatalog,
    blocks: Arc<RwLock<Vec<SearchBlock>>>,
}

impl Default for WordProcessorSearchProvider {
    fn default() -> Self {
        Self {
            catalog: SearchCatalog::new(DOCUMENT_TEXT_KIND),
            blocks: Arc::new(RwLock::new(Vec::new())),
        }
    }
}

impl WordProcessorSearchProvider {
    /// Refresh from the canonical body, never from the widget's bounded render
    /// cache. An unchanged snapshot does not advance the catalogue generation,
    /// so continuation cursors stay valid across ordinary frames.
    pub fn update(&self, doc: &automerge::AutoCommit, containers: &qdoc::Containers) {
        let next = qdoc::model::blocks_in_order(doc, containers)
            .into_iter()
            .filter_map(|id| {
                qdoc::model::block_text(doc, containers, &id).map(|text| SearchBlock { id, text })
            })
            .collect::<Vec<_>>();
        let mut blocks = self
            .blocks
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *blocks == next {
            return;
        }
        let rows = next.iter().map(|block| {
            let resource = block_resource_id(&block.id);
            let result = SearchResult::for_action(
                DOCUMENT_TEXT_KIND,
                preview(&block.text),
                ActionAddress::new(&resource, OPEN_ACTION),
            )
            .with_subtitle("Open document")
            .with_icon(Icon::FileText);
            (resource, block.text.clone(), result)
        });
        // Every row above is canonical and unique by qdoc block id. Keep the
        // old snapshot if a future catalogue invariant rejects an update.
        if self.catalog.replace_all(rows).is_ok() {
            *blocks = next;
        }
    }

    fn position_results(&self, query: &str, results: Vec<SearchResult>) -> Vec<SearchResult> {
        let blocks = self
            .blocks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        results
            .into_iter()
            .filter_map(|mut result| {
                let address = result.action_address()?;
                let block_id = address.resource.strip_prefix(RESOURCE_PREFIX)?;
                let block = blocks.iter().find(|block| block.id == block_id)?;
                let (start, end) = match_range(&block.text, query)?;
                let address =
                    ActionAddress::new(hit_resource_id(block_id, start, end), OPEN_ACTION);
                result.action_id = SearchResult::for_action("", "", &address).action_id;
                result.title = contextual_preview(&block.text, start, end);
                result.subtitle = Some(format!("Characters {}–{}", start + 1, end));
                Some(result)
            })
            .collect()
    }

    fn resource_for_address(&self, address: &ActionAddress) -> Option<Resource> {
        if address.action != OPEN_ACTION {
            return None;
        }
        let (block, start, end) = parse_hit_resource_id(&address.resource)?;
        let blocks = self
            .blocks
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let text = &blocks.iter().find(|entry| entry.id == block)?.text;
        if start > end || end > text.chars().count() {
            return None;
        }
        let operation = WordProcessorOperation::NavigateTo {
            block: block.to_string(),
            start,
            end,
        };
        let action = ActionSpec::operation(
            OPEN_ACTION,
            "Go to text",
            OperationKind::View,
            WORD_PROCESSOR_OPERATION_TYPE,
            &operation,
        )
        .ok()?
        .with_contract(tilo_actions::ActionContract::view());
        Some(
            Resource::new(
                address.resource.clone(),
                "word_processor.text-hit",
                contextual_preview(text, start, end),
            )
            .with_action(action),
        )
    }
}

impl SearchProvider for WordProcessorSearchProvider {
    fn name(&self) -> &str {
        DOCUMENT_TEXT_KIND
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
        let results = self
            .catalog
            .search_limited(query, limit.min(DEFAULT_SEARCH_RESULT_LIMIT));
        self.position_results(query, results)
    }

    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        let mut page = self
            .catalog
            .query_page(query, cursor, work_budget, result_limit);
        page.results = self.position_results(query, page.results);
        page
    }

    fn resolve_action_resource(&self, address: &ActionAddress) -> Option<Resource> {
        self.resource_for_address(address)
    }
}

fn block_resource_id(block: &str) -> String {
    format!("{RESOURCE_PREFIX}{block}")
}

fn hit_resource_id(block: &str, start: usize, end: usize) -> String {
    format!("{RESOURCE_PREFIX}{block}:{start}:{end}")
}

fn parse_hit_resource_id(resource: &str) -> Option<(&str, usize, usize)> {
    let rest = resource.strip_prefix(RESOURCE_PREFIX)?;
    let mut parts = rest.rsplitn(3, ':');
    let end = parts.next()?.parse().ok()?;
    let start = parts.next()?.parse().ok()?;
    let block = parts.next()?;
    (!block.is_empty()).then_some((block, start, end))
}

/// Match in Unicode-lowercased text while returning offsets in original
/// Unicode scalar values. The origin map handles folds which expand one scalar.
fn match_range(text: &str, query: &str) -> Option<(usize, usize)> {
    if query.is_empty() {
        return Some((0, 0));
    }
    let needle = query
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if needle.is_empty() {
        return Some((0, 0));
    }
    let mut folded = String::new();
    let mut origins = Vec::new();
    for (index, scalar) in text.chars().enumerate() {
        for lower in scalar.to_lowercase() {
            folded.push(lower);
            origins.push(index);
        }
    }
    let byte_start = folded.find(&needle)?;
    let folded_start = folded.get(..byte_start)?.chars().count();
    let folded_len = needle.chars().count();
    let start = *origins.get(folded_start)?;
    let end = origins
        .get(folded_start.saturating_add(folded_len).saturating_sub(1))?
        .saturating_add(1);
    Some((start, end))
}

fn preview(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        "Empty paragraph".to_string()
    } else if collapsed.chars().count() > 72 {
        collapsed.chars().take(69).collect::<String>() + "…"
    } else {
        collapsed
    }
}

fn contextual_preview(text: &str, start: usize, end: usize) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let from = start.saturating_sub(28);
    let to = end.saturating_add(44).min(chars.len());
    let mut label = chars
        .get(from..to)
        .unwrap_or(&[])
        .iter()
        .collect::<String>();
    label = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if from > 0 {
        label.insert(0, '…');
    }
    if to < chars.len() {
        label.push('…');
    }
    if label.is_empty() {
        "Empty paragraph".to_string()
    } else {
        label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(lines: &[&str]) -> (automerge::AutoCommit, qdoc::Containers) {
        let mut doc = automerge::AutoCommit::new();
        let containers = qdoc::model::init(&mut doc, &automerge::ROOT);
        let mut ids = qdoc::IdGen::new("search-test".to_string());
        let mut edit = qdoc::DocMut {
            doc: &mut doc,
            c: &containers,
            ids: &mut ids,
        };
        for line in lines {
            let id = edit.insert_block(None, "paragraph");
            edit.splice_text(&id, 0, 0, line);
        }
        (doc, containers)
    }

    #[test]
    fn query_is_position_aware_and_limited() {
        let (doc, containers) = document(&["Alpha needle omega", "Second needle", "no match"]);
        let provider = WordProcessorSearchProvider::default();
        provider.update(&doc, &containers);
        let hits = provider.search_limited("NEEDLE", 1);
        assert_eq!(hits.len(), 1);
        let address = hits[0].action_address().expect("canonical result");
        let (_, start, end) = parse_hit_resource_id(&address.resource).expect("range address");
        assert_eq!((start, end), (6, 12));
    }

    #[test]
    fn refresh_changes_generation_and_invalidates_old_cursor_only_on_change() {
        let (mut doc, containers) = document(&["needle one", "needle two"]);
        let provider = WordProcessorSearchProvider::default();
        provider.update(&doc, &containers);
        let first = provider.search_page("needle", None, 1, 1);
        let cursor = first.next.expect("more results");
        let generation = provider.action_directory_generation();
        provider.update(&doc, &containers);
        assert_eq!(provider.action_directory_generation(), generation);
        assert!(provider.action_directory_cursor_is_current(&cursor));

        let id = qdoc::model::blocks_in_order(&doc, &containers)[0].clone();
        let mut ids = qdoc::IdGen::new("search-edit".to_string());
        qdoc::DocMut {
            doc: &mut doc,
            c: &containers,
            ids: &mut ids,
        }
        .splice_text(&id, 0, 0, "changed ");
        provider.update(&doc, &containers);
        assert_ne!(provider.action_directory_generation(), generation);
        assert!(!provider.action_directory_cursor_is_current(&cursor));
    }

    #[test]
    fn resolved_hit_exports_the_navigation_operation() {
        let (doc, containers) = document(&["before target after"]);
        let provider = WordProcessorSearchProvider::default();
        provider.update(&doc, &containers);
        let address = provider.search("target")[0]
            .action_address()
            .expect("canonical result");
        let resource = provider
            .resolve_action_resource(&address)
            .expect("fresh range resource");
        assert_eq!(resource.kind, "word_processor.text-hit");
        assert_eq!(resource.actions[0].id, OPEN_ACTION);
    }
}
