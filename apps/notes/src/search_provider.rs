//! Notes as a qOS global-search source.
//!
//! Mirrors the reference wiring in [`apps/contacts`]: Notes declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of the
//! page tree, so every note page becomes a typed [`SearchResult`] the unified
//! Ctrl+K palette can rank alongside every other provider's hits. Activating a
//! row addresses the `open` view action exported by that page or block
//! resource. The shared broker therefore owns activation for pointer,
//! keyboard, assistive technology, search and agents alike.
//!
//! The provider is deliberately **library-scoped and synchronous**: its page
//! metadata and plain-text blocks are snapshotted when the search surface
//! opens, so query keystrokes never touch disk or the network.

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

use crate::model::{PageId, PageMeta, Workspace};

/// The kind label (and result-group header) Notes produces.
pub const NOTE_KIND: &str = "Notes";
pub const BLOCK_KIND: &str = "Note blocks";

/// The title shown for a page whose title is still blank.
const UNTITLED: &str = "Untitled";

/// A [`SearchProvider`] over a snapshot of the page tree. Rebuild it (cheap —
/// it clones only the lightweight [`PageMeta`] list, not the bodies) whenever
/// the search surface opens.
pub struct NoteSearchProvider {
    pages: Vec<PageMeta>,
    blocks: std::collections::HashMap<PageId, Vec<crate::actions::NoteBlockResource>>,
}

impl NoteSearchProvider {
    /// Snapshot the current page tree into a provider.
    pub fn new(workspace: &Workspace) -> Self {
        Self {
            pages: workspace
                .pages()
                .iter()
                .take(DEFAULT_SEARCH_INDEX_LIMIT)
                .cloned()
                .collect(),
            blocks: std::collections::HashMap::new(),
        }
    }

    /// Snapshot page metadata together with searchable plain-text blocks.
    pub fn with_blocks(
        workspace: &Workspace,
        blocks: std::collections::HashMap<PageId, Vec<crate::actions::NoteBlockResource>>,
    ) -> Self {
        let pages = workspace
            .pages()
            .iter()
            .take(DEFAULT_SEARCH_INDEX_LIMIT)
            .cloned()
            .collect::<Vec<_>>();
        let mut remaining = DEFAULT_SEARCH_INDEX_LIMIT;
        let blocks = pages
            .iter()
            .filter_map(|page| {
                let rows = blocks.get(&page.id)?;
                let kept = rows.iter().take(remaining).cloned().collect::<Vec<_>>();
                remaining = remaining.saturating_sub(kept.len());
                Some((page.id, kept))
            })
            .collect();
        Self { pages, blocks }
    }

    /// The display title for a page, substituting a placeholder for a blank
    /// title so an untitled page is still findable and readable in the palette.
    fn display_title(page: &PageMeta) -> &str {
        if page.title.trim().is_empty() {
            UNTITLED
        } else {
            page.title.as_str()
        }
    }
}

impl SearchProvider for NoteSearchProvider {
    fn name(&self) -> &str {
        NOTE_KIND
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        if limit == 0 {
            return Vec::new();
        }
        // Substring filter over page titles and block text; the registry
        // re-ranks the survivors by `fuzzy_score` against the visible label.
        // An empty query browses the complete snapshot.
        let q = query.to_lowercase();
        let mut out = Vec::new();
        for page in &self.pages {
            let page_title = Self::display_title(page);
            if page_title.to_lowercase().contains(&q) {
                out.push(
                    SearchResult::for_action(
                        NOTE_KIND,
                        page_title,
                        crate::actions::open_page_address(page.id),
                    )
                    .with_icon(Icon::FileText),
                );
                if out.len() == limit {
                    return out;
                }
            }
            for block in self.blocks.get(&page.id).into_iter().flatten() {
                let collapsed = block.text.split_whitespace().collect::<Vec<_>>().join(" ");
                if collapsed.is_empty() || (!q.is_empty() && !collapsed.to_lowercase().contains(&q))
                {
                    continue;
                }
                let title = if collapsed.chars().count() > 72 {
                    collapsed.chars().take(69).collect::<String>() + "…"
                } else {
                    collapsed
                };
                out.push(
                    SearchResult::for_action(
                        BLOCK_KIND,
                        title,
                        crate::actions::open_block_address(page.id, &block.id),
                    )
                    .with_subtitle(page_title)
                    .with_icon(Icon::List),
                );
                if out.len() == limit {
                    return out;
                }
            }
        }
        out
    }

    fn resolve_action_resource(
        &self,
        address: &qos_actions::ActionAddress,
    ) -> Option<qos_actions::Resource> {
        self.blocks.iter().find_map(|(page, blocks)| {
            blocks
                .iter()
                .find(|block| crate::actions::open_block_address(*page, &block.id) == *address)
                .and_then(|block| crate::actions::block_resource(*page, block))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Workspace;
    use ui::search::SearchRegistry;

    /// A workspace with three titled pages.
    fn populated() -> Workspace {
        let mut ws = Workspace::new();
        let a = ws.create_page(None);
        ws.rename(a, "Grocery list");
        let b = ws.create_page(None);
        ws.rename(b, "Meeting notes");
        let c = ws.create_page(None);
        ws.rename(c, "Trip planning");
        ws
    }

    #[test]
    fn provider_finds_a_real_page_by_title() {
        let ws = populated();
        let provider = NoteSearchProvider::new(&ws);
        let hits = provider.search("meeting");
        assert!(
            hits.iter().any(|r| r.title == "Meeting notes"),
            "the provider surfaces the matching page, got {:?}",
            hits.iter().map(|r| &r.title).collect::<Vec<_>>()
        );
        assert!(
            hits.iter().all(|r| r.kind == NOTE_KIND),
            "all hits are tagged Notes"
        );
    }

    #[test]
    fn empty_query_browses_the_whole_notebook() {
        let ws = populated();
        let provider = NoteSearchProvider::new(&ws);
        assert_eq!(provider.search("").len(), ws.pages().len());
    }

    #[test]
    fn limited_search_honours_the_materialisation_budget() {
        let provider = NoteSearchProvider::new(&populated());
        assert_eq!(provider.search_limited("", 1).len(), 1);
        assert!(provider.search_limited("", 0).is_empty());
    }

    #[test]
    fn page_result_addresses_the_exported_open_action() {
        let ws = populated();
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(NoteSearchProvider::new(&ws)));

        let groups = registry.query_grouped("trip");
        assert_eq!(groups[0].kind, NOTE_KIND);
        let top = &groups[0].results[0];
        assert_eq!(top.title, "Trip planning", "the best match leads");

        assert_eq!(
            top.action_address(),
            Some(crate::actions::open_page_address(
                ws.pages()
                    .iter()
                    .find(|page| page.title == top.title)
                    .unwrap()
                    .id
            )),
            "search and the resource tree publish one canonical address"
        );
    }

    #[test]
    fn block_result_addresses_its_exported_reveal_action() {
        let ws = populated();
        let page = ws
            .pages()
            .iter()
            .find(|page| page.title == "Meeting notes")
            .unwrap();
        let provider = NoteSearchProvider::with_blocks(
            &ws,
            std::collections::HashMap::from([(
                page.id,
                vec![crate::actions::NoteBlockResource {
                    id: "block-object".into(),
                    text: "Budget decisions".into(),
                }],
            )]),
        );
        let hit = provider.search("budget").pop().expect("block hit");
        assert_eq!(hit.kind, BLOCK_KIND);
        assert_eq!(hit.subtitle.as_deref(), Some("Meeting notes"));
        assert_eq!(
            hit.action_address(),
            Some(crate::actions::open_block_address(page.id, "block-object"))
        );
    }

    #[test]
    fn untitled_page_is_still_findable() {
        let mut ws = Workspace::new();
        ws.create_page(None); // left untitled
        let provider = NoteSearchProvider::new(&ws);
        let hits = provider.search("untitled");
        assert_eq!(
            hits.len(),
            1,
            "a blank-titled page surfaces under its placeholder"
        );
    }
}
