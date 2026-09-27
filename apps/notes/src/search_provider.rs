//! Notes as a Tilo global-search source.
//!
//! Mirrors the reference wiring in `apps/contacts`: Notes declares its
//! searchable type by implementing [`SearchProvider`] over a snapshot of the
//! page tree, so every note page becomes a typed [`SearchResult`] the unified
//! Ctrl+K palette can rank alongside every other provider's hits. Activating a
//! row addresses the `open` view action exported by that page or block
//! resource. The shared broker therefore owns activation for pointer,
//! keyboard, assistive technology, search and agents alike.
//!
//! The provider is deliberately **library-scoped and synchronous**: a retained
//! complete catalogue is incrementally updated from the app's in-memory CRDT
//! projection, so opening or querying search never touches disk, network, or
//! the full notebook.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, FieldWeight, SearchCatalog, SearchCursor, SearchPage,
    SearchProvider, SearchRecord, SearchResult,
};

use crate::model::{PageId, PageMeta, Workspace};

/// The kind label (and result-group header) Notes produces.
pub const NOTE_KIND: &str = "Notes";
pub const BLOCK_KIND: &str = "Note blocks";

/// The title shown for a page whose title is still blank.
const UNTITLED: &str = "Untitled";

/// A retained complete catalogue. Cloning this provider when Ctrl+K opens is
/// O(1); the app incrementally updates only changed page/block resources.
#[derive(Clone)]
pub struct NoteSearchProvider {
    catalog: SearchCatalog,
    snapshot: Arc<RwLock<Snapshot>>,
}

#[derive(Default)]
struct Snapshot {
    pages: Vec<PageMeta>,
    blocks: HashMap<PageId, Vec<crate::actions::NoteBlockResource>>,
}

impl NoteSearchProvider {
    /// Snapshot the current page tree into a provider.
    pub fn new(workspace: &Workspace) -> Self {
        Self::with_blocks(workspace, &HashMap::new())
    }

    /// Snapshot page metadata together with searchable plain-text blocks.
    pub fn with_blocks(
        workspace: &Workspace,
        blocks: &HashMap<PageId, Vec<crate::actions::NoteBlockResource>>,
    ) -> Self {
        let mut provider = Self {
            catalog: SearchCatalog::new(NOTE_KIND),
            snapshot: Arc::new(RwLock::new(Snapshot::default())),
        };
        provider.update(workspace, blocks);
        provider
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

    /// Reconcile only changed durable resources into the retained catalogue.
    pub fn update(
        &mut self,
        workspace: &Workspace,
        blocks: &HashMap<PageId, Vec<crate::actions::NoteBlockResource>>,
    ) {
        let pages = workspace.pages().to_vec();
        let blocks = pages
            .iter()
            .filter_map(|page| blocks.get(&page.id).map(|rows| (page.id, rows.clone())))
            .collect::<HashMap<_, _>>();
        let mut snapshot = self
            .snapshot
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let old_pages = snapshot
            .pages
            .iter()
            .map(|page| (page.id, page))
            .collect::<HashMap<_, _>>();
        let new_pages = pages
            .iter()
            .map(|page| (page.id, page))
            .collect::<HashMap<_, _>>();
        for page in old_pages.keys().filter(|id| !new_pages.contains_key(*id)) {
            self.catalog
                .remove(&crate::actions::page_resource_id(*page));
        }
        for (page, old_blocks) in &snapshot.blocks {
            if new_pages.contains_key(page) {
                continue;
            }
            for block in old_blocks {
                self.catalog
                    .remove(&crate::actions::block_resource_id(*page, &block.id));
            }
        }
        for page in &pages {
            let page_changed = old_pages.get(&page.id).copied() != Some(page);
            if page_changed {
                let result = SearchResult::for_action(
                    NOTE_KIND,
                    Self::display_title(page),
                    crate::actions::open_page_address(page.id),
                )
                .with_icon(Icon::FileText);
                let _ = self.catalog.upsert_record(
                    SearchRecord::new(crate::actions::page_resource_id(page.id), result)
                        .field(Self::display_title(page), FieldWeight::TITLE),
                );
            }
            let old_blocks = snapshot.blocks.get(&page.id).cloned().unwrap_or_default();
            let new_blocks = blocks.get(&page.id).cloned().unwrap_or_default();
            let old = old_blocks
                .iter()
                .map(|block| (block.id.as_str(), block))
                .collect::<HashMap<_, _>>();
            let new = new_blocks
                .iter()
                .map(|block| (block.id.as_str(), block))
                .collect::<HashMap<_, _>>();
            for id in old.keys().filter(|id| !new.contains_key(**id)) {
                self.catalog
                    .remove(&crate::actions::block_resource_id(page.id, id));
            }
            for block in &new_blocks {
                if !page_changed && old.get(block.id.as_str()).copied() == Some(block) {
                    continue;
                }
                let resource = crate::actions::block_resource_id(page.id, &block.id);
                let collapsed = block.text.split_whitespace().collect::<Vec<_>>().join(" ");
                if collapsed.is_empty() {
                    self.catalog.remove(&resource);
                    continue;
                }
                let label = if collapsed.chars().count() > 72 {
                    collapsed.chars().take(69).collect::<String>() + "…"
                } else {
                    collapsed
                };
                let result = SearchResult::for_action(
                    BLOCK_KIND,
                    label,
                    crate::actions::open_block_address(page.id, &block.id),
                )
                .with_subtitle(Self::display_title(page))
                .with_icon(Icon::List);
                let _ = self.catalog.upsert_record(
                    SearchRecord::new(resource, result)
                        .field(&block.text, FieldWeight::BODY)
                        .field(Self::display_title(page), FieldWeight::SUBTITLE),
                );
            }
        }
        snapshot.pages = pages;
        snapshot.blocks = blocks;
    }
}

impl SearchProvider for NoteSearchProvider {
    fn name(&self) -> &str {
        NOTE_KIND
    }

    fn action_directory_invocation_domain(&self) -> ui::widget::ActionDirectoryInvocationDomain {
        // This actual retained catalogue owns all omitted search addresses,
        // order and cursors. Its rows target only canonical page/block Open;
        // the owning Notes surface independently versions lazy action dispatch.
        let generation = self.catalog.stats().generation;
        // SearchCatalog saturates its counter. Exhaustion cannot certify an
        // unchanged omitted domain after another catalogue update.
        if generation == u64::MAX {
            return ui::widget::ActionDirectoryInvocationDomain::Unsupported;
        }
        crate::continuation::provider(
            "notes-retained-search-canonical-page-block-open/v1",
            vec![generation.to_be_bytes().to_vec()],
        )
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
        let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
        if limit == 0 {
            return Vec::new();
        }
        self.catalog.search_limited(query, limit)
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
        // Context is derived only for this bounded page of hits. The retained
        // catalogue still owns matching and continuation, including full bodies.
        let snapshot = self
            .snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for result in &mut page.results {
            let Some(address) = result.action_address() else {
                continue;
            };
            let Some((note, block)) = crate::actions::parse_block_resource_id(&address.resource)
            else {
                continue;
            };
            let Some(block) = snapshot
                .blocks
                .get(&note)
                .and_then(|blocks| blocks.iter().find(|candidate| candidate.id == block))
            else {
                continue;
            };
            let collapsed = block.text.split_whitespace().collect::<Vec<_>>().join(" ");
            let lower = collapsed.to_lowercase();
            let needle = query.to_lowercase();
            let offset = lower
                .find(needle.trim())
                .or_else(|| needle.split_whitespace().find_map(|word| lower.find(word)))
                .unwrap_or(0);
            let desired_start = lower
                .get(..offset)
                .map_or(0, |prefix| prefix.chars().count())
                .saturating_sub(8);
            // Short blocks need no excerpt. For long blocks, start at a
            // word boundary so context never opens with a chopped word.
            let start = if collapsed.chars().count() <= 100 {
                0
            } else {
                collapsed
                    .chars()
                    .take(desired_start)
                    .enumerate()
                    .filter_map(|(index, ch)| ch.is_whitespace().then_some(index.saturating_add(1)))
                    .last()
                    .unwrap_or(0)
            };
            let excerpt = collapsed.chars().skip(start).take(100).collect::<String>();
            result.title = format!(
                "{}{}{}",
                if start > 0 { "…" } else { "" },
                excerpt,
                if collapsed.chars().count() > start.saturating_add(100) {
                    "…"
                } else {
                    ""
                }
            );
        }
        drop(snapshot);
        page
    }

    fn resolve_action_resource(
        &self,
        address: &tilo_actions::ActionAddress,
    ) -> Option<tilo_actions::Resource> {
        let snapshot = self
            .snapshot
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(page) = snapshot
            .pages
            .iter()
            .find(|page| crate::actions::open_page_address(page.id) == *address)
        {
            return Some(crate::actions::page_resource(
                page,
                &snapshot.pages,
                &automerge::AutoCommit::new(),
            ));
        }
        snapshot.blocks.iter().find_map(|(page, blocks)| {
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
    use ui::search::testing::{LATE_HIT_NEEDLE, assert_late_hit_reachable};

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
    fn search_finds_titled_untitled_and_browsed_pages_within_budget() {
        eprintln!("phase provider_finds_a_real_page_by_title");
        {
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

        eprintln!("phase untitled_page_is_still_findable");
        {
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

        eprintln!("phase empty_query_browses_the_whole_notebook");
        {
            let ws = populated();
            let provider = NoteSearchProvider::new(&ws);
            assert_eq!(provider.search("").len(), ws.pages().len());
        }

        eprintln!("phase limited_search_honours_the_materialisation_budget");
        {
            let provider = NoteSearchProvider::new(&populated());
            assert_eq!(provider.search_limited("", 1).len(), 1);
            assert!(provider.search_limited("", 0).is_empty());
        }
    }

    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn search_results_address_exported_actions_with_bounded_snippets() {
        eprintln!("phase page_result_addresses_the_exported_open_action");
        {
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

        eprintln!("phase block_result_addresses_its_exported_reveal_action");
        {
            let ws = populated();
            let page = ws
                .pages()
                .iter()
                .find(|page| page.title == "Meeting notes")
                .unwrap();
            let provider = NoteSearchProvider::with_blocks(
                &ws,
                &std::collections::HashMap::from([(
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

        eprintln!("phase contextual_body_snippets_preserve_short_blocks_and_word_boundaries");
        {
            let ws = populated();
            let page = ws.pages()[0].id;
            let long = format!(
                "{}Write the migration guide with the release team",
                "Earlier context. ".repeat(12)
            );
            let provider = NoteSearchProvider::with_blocks(
                &ws,
                &HashMap::from([(
                    page,
                    vec![
                        crate::actions::NoteBlockResource {
                            id: "short".into(),
                            text: "Write the migration guide".into(),
                        },
                        crate::actions::NoteBlockResource {
                            id: "long".into(),
                            text: long,
                        },
                    ],
                )]),
            );
            let hits = provider.search_page("migration", None, 512, 64).results;
            let short = hits
                .iter()
                .find(|hit| {
                    hit.action_address() == Some(crate::actions::open_block_address(page, "short"))
                })
                .unwrap();
            assert_eq!(short.title, "Write the migration guide");
            let long = hits
                .iter()
                .find(|hit| {
                    hit.action_address() == Some(crate::actions::open_block_address(page, "long"))
                })
                .unwrap();
            assert_eq!(
                long.title,
                "…Write the migration guide with the release team"
            );
        }

        eprintln!("phase retained_catalog_pages_adversarial_queries_without_omitting_late_blocks");
        {
            let (provider, hit) = assert_late_hit_reachable(
                |corpus| {
                    let ws = populated();
                    let page = ws.pages()[0].id;
                    let blocks = corpus
                        .map(|row| crate::actions::NoteBlockResource {
                            id: uuid::Uuid::new_v4().to_string(),
                            text: row.text,
                        })
                        .collect();
                    NoteSearchProvider::with_blocks(&ws, &HashMap::from([(page, blocks)]))
                },
                3_000,
            );
            assert_eq!(hit.title, format!("ordinary record 2999 {LATE_HIT_NEEDLE}"));
            let first = provider.search_page("ordinary", None, 7, 3);
            assert!(first.next.is_some(), "broad query exposes a continuation");
            assert!(first.inspected <= 7 && first.results.len() <= 3);
            let second = provider.search_page("ordinary", first.next.as_ref(), 7, 3);
            assert!(second.inspected <= 7 && second.results.len() <= 3);
        }
    }
}
