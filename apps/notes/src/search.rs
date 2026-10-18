//! What the notebook's search finds: every note by its title, and every
//! block of every note by its words.
//!
//! These are the notebook resource's own search entries
//! ([`Resource::search`](ui::tilo_resource::Resource::search)), so the one
//! search source the framework indexes for the app lists them, pages through
//! them however many there are, and activates a hit through the `open` view
//! action the page or block publishes.

use ui::tilo_resource::SearchEntry;

use crate::notebook::{Notebook, Page, page_of};

/// The result group notes are listed under.
#[must_use]
pub fn note_kind() -> String {
    ui::i18n!("notes-search-notes")
}

/// The result group blocks are listed under.
#[must_use]
pub fn block_kind() -> String {
    ui::i18n!("notes-search-blocks")
}

/// The longest block label a hit shows; the whole block is still searched.
const BLOCK_LABEL_CHARS: usize = 72;

/// The display title for a page, substituting a placeholder for a blank
/// title so an untitled page is still findable and readable in the palette.
fn display_title(page: &Page) -> &str {
    if page.title.trim().is_empty() {
        crate::notebook::UNTITLED
    } else {
        page.title.as_str()
    }
}

/// Every note and every non-empty block of `notebook`, each opening its
/// note.
pub fn entries(notebook: &Notebook) -> Vec<SearchEntry> {
    let mut entries = Vec::new();
    for (id, page) in notebook.pages.iter() {
        let Some(page_id) = page_of(id) else {
            continue;
        };
        let anchor = crate::actions::page_resource_id(page_id);
        let title = display_title(page);
        entries.push(
            SearchEntry::new(title, "")
                .anchored(anchor.clone())
                .grouped(note_kind())
                .with_icon("file-text"),
        );
        for block in page.blocks() {
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
                    .anchored(anchor.clone())
                    .grouped(block_kind())
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
    use crate::model::PageId;
    use ui::search::SearchProvider;
    use ui::tilo_resource::{Invoke, Replica};
    use ui::widget::BlockContent;

    /// A notebook with three titled notes.
    fn populated() -> (Replica<Notebook>, Vec<PageId>) {
        let mut notebook = Replica::<Notebook>::new();
        let pages: Vec<PageId> = ["Grocery list", "Meeting notes", "Trip planning"]
            .into_iter()
            .map(|title| add(&mut notebook, title))
            .collect();
        (notebook, pages)
    }

    fn add(notebook: &mut Replica<Notebook>, title: &str) -> PageId {
        let id = PageId::new_v4();
        notebook
            .apply_action(&crate::actions::create_page_action(id, None, title))
            .unwrap();
        id
    }

    /// `page`'s body made of paragraphs `texts`.
    fn with_blocks(notebook: &mut Replica<Notebook>, page: PageId, texts: &[&str]) {
        let blocks: Vec<BlockContent> = texts
            .iter()
            .map(|text| BlockContent::paragraph("", *text))
            .collect();
        notebook
            .apply(Invoke::apply(&crate::actions::write_body_action(page, &blocks)).unwrap())
            .unwrap();
    }

    fn search(notebook: &Replica<Notebook>) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<Notebook>(entries(&notebook.state().unwrap()))
    }

    #[test]
    fn search_finds_titled_and_untitled_pages_within_budget() {
        {
            let hits = search(&populated().0).search("meeting");
            assert!(
                hits.iter().any(|r| r.title == "Meeting notes"),
                "the source surfaces the matching page, got {:?}",
                hits.iter().map(|r| &r.title).collect::<Vec<_>>()
            );
            assert!(
                hits.iter().all(|r| r.kind == note_kind()),
                "all hits are listed under Notes"
            );
        }
        {
            let mut notebook = Replica::<Notebook>::new();
            add(&mut notebook, "");
            let hits = search(&notebook).search("untitled");
            assert_eq!(
                hits.len(),
                1,
                "a blank-titled page surfaces under its placeholder"
            );
        }
        {
            let source = search(&populated().0);
            assert_eq!(source.search_limited("trip", 1).len(), 1);
            assert!(source.search_limited("trip", 0).is_empty());
        }
    }

    #[test]
    fn search_results_open_the_note_they_are_in() {
        {
            let (notebook, pages) = populated();
            let top = search(&notebook).search("trip").remove(0);
            assert_eq!(top.title, "Trip planning", "the best match leads");
            assert_eq!(
                top.action_id,
                crate::actions::page_resource_id(pages[2]),
                "a hit opens the note's item"
            );
        }
        {
            let (mut notebook, pages) = populated();
            with_blocks(&mut notebook, pages[1], &["Budget decisions"]);
            let hit = search(&notebook)
                .search("budget")
                .into_iter()
                .find(|hit| hit.kind == block_kind())
                .expect("block hit");
            assert_eq!(hit.subtitle.as_deref(), Some("Meeting notes"));
            assert_eq!(hit.action_id, crate::actions::page_resource_id(pages[1]));
        }
        {
            // A long block reads as its opening words; the rest is still found.
            let long = format!(
                "{}Write the migration guide with the release team",
                "Earlier context. ".repeat(12)
            );
            let (mut notebook, pages) = populated();
            with_blocks(&mut notebook, pages[0], &[&long]);
            let hit = search(&notebook)
                .search("release team")
                .into_iter()
                .find(|hit| hit.kind == block_kind())
                .expect("a match late in a long block is found");
            assert!(hit.title.ends_with('…'));
            assert!(hit.title.chars().count() <= BLOCK_LABEL_CHARS);
        }
    }

    #[test]
    fn every_block_stays_reachable_page_by_page() {
        let texts: Vec<String> = (0..3_000).map(|n| format!("ordinary record {n}")).collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let (mut notebook, pages) = populated();
        with_blocks(&mut notebook, pages[0], &refs);
        let source = search(&notebook);
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
