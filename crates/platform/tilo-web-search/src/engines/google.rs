//! Google. Ported from metasearch2's `src/engines/search/google.rs`
//! (CC0-1.0); its featured-snippet, image and autocomplete readers are not.
//!
//! Google answers a client it does not take for a browser with a page that
//! needs JavaScript, which [`super::blocked_reason`] reports as blocked, so
//! this engine is asked only when a caller names it.

use url::Url;

use super::{EngineError, EngineRequest, EngineResult, Layout, Query, query_param};
use crate::html::Document;

pub(super) fn request(query: &str) -> Option<EngineRequest> {
    EngineRequest::get(
        "https://www.google.com/search",
        &[
            ("q", query),
            // `nfpr` stops Google from answering a corrected query instead.
            ("nfpr", "1"),
            ("filter", "0"),
            ("start", "0"),
        ],
    )
}

pub(super) fn parse(doc: &Document) -> Result<Vec<EngineResult>, EngineError> {
    Layout {
        container: "#search, #rso, #main",
        // Ads carry `xpd` too, but never this controller.
        result: "[jscontroller=SC7lYd]",
        title: Query::Text("h3"),
        href: Query::Href("a[href]"),
        snippet: Query::Text(
            "div[data-sncf='2'], div[data-sncf='1,2'], div[style='-webkit-line-clamp:2']",
        ),
        clean_href,
    }
    .parse(doc)
}

/// Google sometimes links through `/url?q=<address>`.
fn clean_href(href: &str) -> Option<String> {
    if href.starts_with("/url?") {
        let parsed = Url::parse(&format!("https://www.google.com{href}")).ok()?;
        return crate::urls::clean(&query_param(&parsed, "q")?);
    }
    crate::urls::clean(href)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_links_resolve_to_the_page() {
        assert_eq!(
            clean_href("/url?q=https://example.org/a%3Fb&sa=U").as_deref(),
            Some("https://example.org/a?b")
        );
    }
}
