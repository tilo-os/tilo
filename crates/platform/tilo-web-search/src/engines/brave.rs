//! Brave Search. Ported from metasearch2's `src/engines/search/brave.rs`
//! (CC0-1.0).

use super::{EngineError, EngineRequest, EngineResult, Layout, Query, plain_href};
use crate::html::Document;

pub(super) fn request(query: &str) -> Option<EngineRequest> {
    EngineRequest::get("https://search.brave.com/search", &[("q", query)])
}

pub(super) fn parse(doc: &Document) -> Result<Vec<EngineResult>, EngineError> {
    Layout {
        container: "main",
        result: "main .snippet[data-pos]:not(.standalone)",
        title: Query::Text(".title"),
        href: Query::Href("a"),
        // Forum threads show the opening post instead of a generic snippet.
        snippet: Query::Text(
            ".generic-snippet, .video-snippet > .snippet-description, .inline-qa-question",
        ),
        clean_href: plain_href,
    }
    .parse(doc)
}
