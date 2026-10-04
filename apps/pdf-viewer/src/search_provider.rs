//! Retained global-search projection for the open PDF: what the document
//! itself says — its pages' text, its outline — and the current find hits.
//! The markup people made on it is searched through the resource
//! ([`crate::PdfViewer`]'s `search`), so each entity has one search source.

use std::collections::HashSet;
use ui::search::{CatalogSearchProvider, SearchCatalog, SearchCatalogError, SearchResult};

use tilo_actions::ActionAddress;

use crate::actions::{PdfResourceCtx, SEARCH_RESULT_RESOURCE_LIMIT, outline_resource_id};
use crate::model::Page;

pub const PDF_SEARCH_KIND: &str = "PDF Viewer";

#[derive(Clone)]
pub struct PdfSearchProvider {
    catalog: SearchCatalog,
}

impl PdfSearchProvider {
    pub fn from_catalog(catalog: SearchCatalog) -> Self {
        Self { catalog }
    }

    /// Update the retained catalog. Canonical address errors deliberately
    /// propagate, preventing a silent hole in global search.
    ///
    /// # Errors
    ///
    /// Returns a catalog error if a stable id is invalid or a result does not
    /// use an action address for exactly that resource.
    #[allow(
        clippy::too_many_lines,
        reason = "one retained index projection keeps every canonical PDF resource visible for global search"
    )]
    pub fn index_into(
        catalog: &SearchCatalog,
        ctx: &PdfResourceCtx,
        source_pages: &[Page],
    ) -> Result<HashSet<String>, SearchCatalogError> {
        let mut ids = HashSet::new();
        let document_id = <crate::PdfViewer as ui::tilo_resource::Resource>::KIND.to_string();
        catalog.upsert(
            document_id.clone(),
            format!("{}\0pdf document\0find\0save annotated PDF", ctx.title),
            SearchResult::for_action(
                PDF_SEARCH_KIND,
                &ctx.title,
                ActionAddress::new(&document_id, "find"),
            )
            .with_subtitle("Open PDF document"),
        )?;
        ids.insert(document_id);
        for (index, page_id) in ctx.page_ids.iter().enumerate() {
            let page = index + 1;
            let resource = format!("pdf.page:{page_id}");
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
            catalog.upsert(
                resource.clone(),
                format!("Page {page}\0pdf page\0open\0{text}"),
                SearchResult::for_action(
                    PDF_SEARCH_KIND,
                    format!("Page {page}"),
                    ActionAddress::new(&resource, "open"),
                )
                .with_subtitle(text.chars().take(120).collect::<String>()),
            )?;
            ids.insert(resource);
        }
        // Imported PDF bookmarks / outline entries are first-class navigation
        // resources too.  Keep their search address identical to the resource
        // tree, so a global-search result continues to open the intended
        // section rather than merely matching the page text that happens to
        // contain the same title.
        for (index, (title, page, level)) in ctx.outline.iter().enumerate() {
            let Some(resource) = outline_resource_id(ctx, index) else {
                continue;
            };
            catalog.upsert(
                resource.clone(),
                format!(
                    "bookmark\0outline\0contents\0{title}\0page {page}\0level {}\0open",
                    usize::from(*level) + 1
                ),
                SearchResult::for_action(
                    PDF_SEARCH_KIND,
                    format!("Bookmark — {title}"),
                    ActionAddress::new(&resource, "open"),
                )
                .with_subtitle(format!("Contents · page {page}")),
            )?;
            ids.insert(resource);
        }
        // The current find query is local view state, but its materialised
        // results are still first-class, canonical resources.  Keep this
        // projection bounded and use the same ids/actions as Ctrl+K's resource
        // tree so a global-search hit cannot accidentally navigate to a later
        // query's ordinal match.
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
                catalog.upsert(
                    resource.clone(),
                    format!(
                        "find result\0{}\0page {page}\0match {}\0open",
                        ctx.search_query,
                        index + 1
                    ),
                    SearchResult::for_action(
                        PDF_SEARCH_KIND,
                        format!("Match {} · page {page}", index + 1),
                        ActionAddress::new(&resource, "open"),
                    )
                    .with_subtitle(format!("Find result for “{}”", ctx.search_query)),
                )?;
                ids.insert(resource);
            }
        }
        Ok(ids)
    }
}

impl CatalogSearchProvider for PdfSearchProvider {
    fn provider_name(&self) -> &str {
        PDF_SEARCH_KIND
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }
}
