//! What the open PDF itself says, for search: its pages' text, its outline
//! and the current find hits. The surface publishes these beside the
//! resource's own entries (the markup people made on it, [`crate::PdfMarkup`]'s
//! `search`), so each entity has one search source.

use tilo_actions::ActionAddress;
use ui::tilo_resource::SearchEntry;

use crate::actions::{PdfResourceCtx, SEARCH_RESULT_RESOURCE_LIMIT, outline_resource_id};
use crate::model::Page;

pub const PDF_SEARCH_KIND: &str = "PDF Viewer";

/// One hit: listed with the document's, invoking `address`.
fn entry(title: String, text: String, subtitle: String, address: ActionAddress) -> SearchEntry {
    SearchEntry::new(title, text)
        .anchored(address.resource.clone())
        .invoking(address)
        .grouped(PDF_SEARCH_KIND)
        .subtitled(subtitle)
}

/// The search entries of the document `ctx` shows, its pages' text read from
/// `source_pages`. Every hit invokes the canonical action of the resource it
/// names, the same address the published tree carries.
pub fn surface_entries(ctx: &PdfResourceCtx, source_pages: &[Page]) -> Vec<SearchEntry> {
    let mut entries = Vec::new();
    let document_id = <crate::PdfMarkup as ui::tilo_resource::Resource>::KIND.to_string();
    entries.push(entry(
        ctx.title.clone(),
        "pdf document find save annotated PDF".into(),
        "Open PDF document".into(),
        ActionAddress::new(&document_id, "find"),
    ));
    for (index, page_id) in ctx.page_ids.iter().enumerate() {
        let page = index + 1;
        let resource = ui::tilo_resource::items::item_resource_id::<crate::PdfMarkup>(
            "pages",
            &ui::tilo_resource::Id::from_raw(page_id.as_str()),
        );
        let source_page = ctx.page_sources.get(index).copied().unwrap_or(page);
        let text = source_pages
            .get(source_page.saturating_sub(1))
            .map(|page| {
                page.blocks
                    .iter()
                    .filter_map(crate::model::Block::search_text)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let subtitle = text.chars().take(120).collect::<String>();
        // A page opens as the page it is.
        entries.push(
            SearchEntry::new(format!("Page {page}"), format!("pdf page open {text}"))
                .anchored(resource)
                .grouped(PDF_SEARCH_KIND)
                .subtitled(subtitle),
        );
    }
    // Imported PDF bookmarks / outline entries are first-class navigation
    // resources too. Keep their search address identical to the resource
    // tree, so a search result opens the intended section rather than merely
    // matching the page text that happens to contain the same title.
    for (index, (title, page, level)) in ctx.outline.iter().enumerate() {
        let Some(resource) = outline_resource_id(ctx, index) else {
            continue;
        };
        entries.push(entry(
            format!("Bookmark — {title}"),
            format!(
                "bookmark outline contents {title} page {page} level {} open",
                usize::from(*level) + 1
            ),
            format!("Contents on page {page}"),
            ActionAddress::new(&resource, "go-to"),
        ));
    }
    // The current find query is local view state, but its results are
    // canonical resources. Keep this bounded and use the same ids and actions
    // as the published tree, so a hit cannot navigate to a later query's
    // ordinal match.
    if !ctx.search_query.trim().is_empty() {
        for (index, page) in ctx
            .search_pages
            .iter()
            .copied()
            .take(SEARCH_RESULT_RESOURCE_LIMIT)
            .enumerate()
        {
            let resource = format!(
                "pdf.search-result:{}:{}:{index}:{page}",
                ctx.doc_id, ctx.search_token
            );
            entries.push(entry(
                format!("Match {} on page {page}", index + 1),
                format!(
                    "find result {} page {page} match {} open",
                    ctx.search_query,
                    index + 1
                ),
                format!("Find result for “{}”", ctx.search_query),
                ActionAddress::new(&resource, "go-to-match"),
            ));
        }
    }
    entries
}
