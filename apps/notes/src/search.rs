//! What the notebook's search finds: every note by its title, and every
//! block of every note by its words.
//!
//! These are the notebook resource's own search entries
//! ([`Resource::search`](ui::tilo_resource::Resource::search)), so the one
//! search source the framework indexes for the app lists them, pages through
//! them however many there are, and activates a hit through the `open` view
//! action the page or block publishes.

use ui::tilo_resource::SearchEntry;

use crate::model::{PageMeta, Workspace};

/// The result group notes are listed under.
pub const NOTE_KIND: &str = "Notes";
/// The result group blocks are listed under.
pub const BLOCK_KIND: &str = "Note blocks";

/// The title shown for a page whose title is still blank.
const UNTITLED: &str = "Untitled";

/// The longest block label a hit shows; the whole block is still searched.
const BLOCK_LABEL_CHARS: usize = 72;

/// The display title for a page, substituting a placeholder for a blank
/// title so an untitled page is still findable and readable in the palette.
fn display_title(page: &PageMeta) -> &str {
    if page.title.trim().is_empty() {
        UNTITLED
    } else {
        page.title.as_str()
    }
}

/// Every page and every non-empty block of `workspace`.
pub fn entries(workspace: &Workspace) -> Vec<SearchEntry> {
    let mut entries = Vec::new();
    for page in workspace.pages() {
        let title = display_title(page);
        entries.push(
            SearchEntry::new(title, "")
                .anchored(crate::actions::page_resource_id(page.id))
                .invoking(crate::actions::open_page_address(page.id))
                .grouped(NOTE_KIND)
                .with_icon("file-text"),
        );
        for block in workspace.bodies.get(&page.id).into_iter().flatten() {
            let collapsed = block.text.split_whitespace().collect::<Vec<_>>().join(" ");
            if collapsed.is_empty() {
                continue;
            }
            let label = if collapsed.chars().count() > BLOCK_LABEL_CHARS {
                collapsed
                    .chars()
                    .take(BLOCK_LABEL_CHARS - 3)
                    .collect::<String>()
                    + "…"
            } else {
                collapsed
            };
            entries.push(
                SearchEntry::new(label, block.text.clone())
                    .anchored(crate::actions::block_resource_id(page.id, &block.id))
                    .invoking(crate::actions::open_block_address(page.id, &block.id))
                    .grouped(BLOCK_KIND)
                    .subtitled(title)
                    .with_icon("list"),
            );
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::StoredBlock;
    use crate::model::Workspace;
    use crate::resource::Notebook;
    use ui::search::SearchProvider;

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

    /// `ws` with `blocks` as the body of its `page`th page.
    fn with_blocks(mut ws: Workspace, page: usize, blocks: &[(&str, &str)]) -> Workspace {
        let id = ws.pages()[page].id;
        let mut bodies = (*ws.bodies).clone();
        bodies.insert(
            id,
            blocks
                .iter()
                .map(|(id, text)| StoredBlock {
                    id: (*id).into(),
                    kind: ui::widget::BlockKind::Paragraph,
                    text: (*text).into(),
                    marks: Vec::new(),
                    nesting: 0,
                    code_group: None,
                })
                .collect(),
        );
        ws.bodies = std::sync::Arc::new(bodies);
        ws
    }

    fn search(ws: &Workspace) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<Notebook>(entries(ws))
    }

    #[test]
    fn search_finds_titled_and_untitled_pages_within_budget() {
        {
            let hits = search(&populated()).search("meeting");
            assert!(
                hits.iter().any(|r| r.title == "Meeting notes"),
                "the source surfaces the matching page, got {:?}",
                hits.iter().map(|r| &r.title).collect::<Vec<_>>()
            );
            assert!(
                hits.iter().all(|r| r.kind == NOTE_KIND),
                "all hits are listed under Notes"
            );
        }
        {
            let mut ws = Workspace::new();
            ws.create_page(None); // left untitled
            let hits = search(&ws).search("untitled");
            assert_eq!(
                hits.len(),
                1,
                "a blank-titled page surfaces under its placeholder"
            );
        }
        {
            let source = search(&populated());
            assert_eq!(source.search_limited("trip", 1).len(), 1);
            assert!(source.search_limited("trip", 0).is_empty());
        }
    }

    #[test]
    fn search_results_address_exported_actions() {
        {
            let ws = populated();
            let top = search(&ws).search("trip").remove(0);
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
        {
            let ws = with_blocks(populated(), 1, &[("block-object", "Budget decisions")]);
            let page = ws.pages()[1].id;
            let hit = search(&ws)
                .search("budget")
                .into_iter()
                .find(|hit| hit.kind == BLOCK_KIND)
                .expect("block hit");
            assert_eq!(hit.subtitle.as_deref(), Some(ws.pages()[1].title.as_str()));
            assert_eq!(
                hit.action_address(),
                Some(crate::actions::open_block_address(page, "block-object"))
            );
        }
        {
            // A long block reads as its opening words; the rest is still found.
            let long = format!(
                "{}Write the migration guide with the release team",
                "Earlier context. ".repeat(12)
            );
            let ws = with_blocks(populated(), 0, &[("long", &long)]);
            let hit = search(&ws)
                .search("release team")
                .into_iter()
                .find(|hit| hit.kind == BLOCK_KIND)
                .expect("a match late in a long block is found");
            assert!(hit.title.ends_with('…'));
            assert!(hit.title.chars().count() <= BLOCK_LABEL_CHARS);
        }
    }

    #[test]
    fn every_block_stays_reachable_page_by_page() {
        let blocks: Vec<(String, String)> = (0..3_000)
            .map(|n| (format!("b{n}"), format!("ordinary record {n}")))
            .collect();
        let refs: Vec<(&str, &str)> = blocks
            .iter()
            .map(|(id, text)| (id.as_str(), text.as_str()))
            .collect();
        let ws = with_blocks(populated(), 0, &refs);
        let source = search(&ws);
        let first =
            source.search_page(&ui::search::SearchQuery::parse("ordinary", &[]), None, 7, 3);
        assert!(first.next.is_some(), "a broad query continues");
        assert!(first.results.len() <= 3);
        assert!(
            source
                .search("record 2999")
                .iter()
                .any(|hit| hit.title == "ordinary record 2999"),
            "the last block is as findable as the first"
        );
    }
}
