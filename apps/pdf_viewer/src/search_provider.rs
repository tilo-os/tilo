//! Retained global-search projection for the open PDF.

use std::collections::HashSet;

use qos_actions::ActionAddress;
use ui::search::{
    DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchCatalogError, SearchCursor, SearchPage,
    SearchProvider, SearchResult,
};

use crate::actions::PdfResourceCtx;
use crate::model::{Annotation, Comment, Page, Signature};

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
    pub fn index_into(
        catalog: &SearchCatalog,
        ctx: &PdfResourceCtx,
        source_pages: &[Page],
        annotations: &[Annotation],
        comments: &[Comment],
        signatures: &[Signature],
    ) -> Result<HashSet<String>, SearchCatalogError> {
        let mut ids = HashSet::new();
        let document_id = format!("pdf.document:{}", ctx.doc_id);
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
            let resource = format!("pdf.page:{}:{page_id}", ctx.doc_id);
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
        for annotation in annotations {
            let resource = format!("pdf.annotation:{}", annotation.id);
            catalog.upsert(
                resource.clone(),
                format!(
                    "annotation\0{}\0page {}\0{}\0open",
                    annotation.kind.type_key(),
                    annotation.page,
                    annotation.author
                ),
                SearchResult::for_action(
                    PDF_SEARCH_KIND,
                    format!("{} annotation", annotation.kind.type_key()),
                    ActionAddress::new(&resource, "open"),
                )
                .with_subtitle(format!("Page {} · {}", annotation.page, annotation.author)),
            )?;
            ids.insert(resource);
        }
        for comment in comments {
            let resource = format!("pdf.comment:{}", comment.id);
            catalog.upsert(
                resource.clone(),
                format!(
                    "comment\0{}\0{}\0page {}\0open\0reply\0resolve",
                    comment.author, comment.body, comment.page
                ),
                SearchResult::for_action(
                    PDF_SEARCH_KIND,
                    format!("Comment — {}", comment.author),
                    ActionAddress::new(&resource, "open"),
                )
                .with_subtitle(comment.body.clone()),
            )?;
            ids.insert(resource);
        }
        for signature in signatures {
            let resource = format!("pdf.signature:{}", signature.id);
            catalog.upsert(
                resource.clone(),
                format!(
                    "signature\0{}\0{}\0page {}\0open",
                    signature.signer, signature.date, signature.page
                ),
                SearchResult::for_action(
                    PDF_SEARCH_KIND,
                    format!("Signature by {}", signature.signer),
                    ActionAddress::new(&resource, "open"),
                )
                .with_subtitle(format!("Page {} · {}", signature.page, signature.date)),
            )?;
            ids.insert(resource);
        }
        Ok(ids)
    }
}

impl SearchProvider for PdfSearchProvider {
    fn name(&self) -> &str {
        PDF_SEARCH_KIND
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
        self.catalog.search_limited(query, limit)
    }
    fn search_page(
        &self,
        query: &str,
        cursor: Option<&SearchCursor>,
        work_budget: usize,
        result_limit: usize,
    ) -> SearchPage {
        self.catalog
            .query_page(query, cursor, work_budget, result_limit)
    }
}
