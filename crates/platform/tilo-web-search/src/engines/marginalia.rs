//! Marginalia Search, an independent index of the small web. Ported from
//! metasearch2's `src/engines/search/marginalia.rs` (CC0-1.0).

use super::{EngineError, EngineRequest, EngineResult, Layout, Query, plain_href};
use crate::html::Document;

/// As in metasearch2: Marginalia is good at short keyword queries and noise
/// for anything longer, so it is not asked those.
pub(super) fn request(query: &str) -> Option<EngineRequest> {
    if query.split_whitespace().count() > 3
        || !query.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ')
    {
        return None;
    }
    EngineRequest::get(
        "https://old-search.marginalia.nu/search",
        &[
            ("query", query),
            ("profile", "corpo"),
            ("js", "default"),
            ("adtech", "default"),
        ],
    )
}

pub(super) fn parse(doc: &Document) -> Result<Vec<EngineResult>, EngineError> {
    Layout {
        // Its page has no result-list wrapper; a page without one result
        // section is not a results page this reader understands.
        container: "section.search-result",
        result: "section.search-result",
        title: Query::Text("h2"),
        href: Query::Href("a[href]"),
        snippet: Query::Text("p.description"),
        clean_href: plain_href,
    }
    .parse(doc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_short_keyword_queries_are_asked() {
        assert!(request("rust language").is_some());
        assert!(request("how do i write rust code").is_none());
        assert!(request("c++").is_none());
    }
}
